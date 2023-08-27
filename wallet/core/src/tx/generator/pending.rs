use crate::result::Result;
use crate::tx::Generator;
use crate::utxo::UtxoEntryReference;
use crate::DynRpcApi;
use kaspa_addresses::Address;
use kaspa_consensus_core::sign::sign_with_multiple_v2;
use kaspa_consensus_core::tx::{SignableTransaction, Transaction, TransactionId};
use kaspa_rpc_core::{RpcTransaction, RpcTransactionId};
use std::sync::Mutex;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use workflow_log::log_info;

pub(crate) struct PendingTransactionInner {
    /// Generator that produced the transaction
    pub(crate) generator: Generator,
    /// UtxoEntryReferences of the pending transaction
    pub(crate) utxo_entries: Vec<UtxoEntryReference>,
    /// Signable transaction (actual transaction that will be signed and sent)
    pub(crate) signable_tx: Mutex<SignableTransaction>,
    /// UTXO addresses used by this transaction
    pub(crate) addresses: Vec<Address>,
    /// Whether the transaction has been committed to the mempool via RPC
    pub(crate) is_committed: AtomicBool,
    /// Payment value of the transaction (transaction destination amount)
    pub(crate) payment_value: Option<u64>,
    /// Change value of the transaction (transaction change amount)
    pub(crate) change_value: u64,
    /// Total aggregate value of all inputs
    pub(crate) aggregate_input_value: u64,
    /// Total aggregate value of all outputs
    pub(crate) aggregate_output_value: u64,
    /// Fees of the transaction
    pub(crate) fees: u64,
    /// Whether the transaction is a final or a batch transaction
    pub(crate) is_final: bool,
}

/// Meta transaction encapsulating a transaction generated by the [`Generator`].
/// Contains auxiliary information about the transaction such as aggergate
/// input/output amounts, fees, etc.
#[derive(Clone)]
pub struct PendingTransaction {
    pub(crate) inner: Arc<PendingTransactionInner>,
}

impl PendingTransaction {
    pub fn try_new(
        generator: &Generator,
        transaction: Transaction,
        utxo_entries: Vec<UtxoEntryReference>,
        addresses: Vec<Address>,
        payment_value: Option<u64>,
        change_value: u64,
        aggregate_input_value: u64,
        aggregate_output_value: u64,
        fees: u64,
        is_final: bool,
    ) -> Result<Self> {
        let entries = utxo_entries.iter().map(|e| e.utxo.entry.clone()).collect::<Vec<_>>();
        let signable_tx = Mutex::new(SignableTransaction::with_entries(transaction, entries));
        Ok(Self {
            inner: Arc::new(PendingTransactionInner {
                generator: generator.clone(),
                signable_tx,
                utxo_entries,
                addresses,
                is_committed: AtomicBool::new(false),
                payment_value,
                change_value,
                aggregate_input_value,
                aggregate_output_value,
                fees,
                is_final,
            }),
        })
    }

    pub fn id(&self) -> TransactionId {
        self.inner.signable_tx.lock().unwrap().id()
    }

    /// Addresses used by the pending transaction
    pub fn addresses(&self) -> &Vec<Address> {
        &self.inner.addresses
    }

    /// Get UTXO entries [`Vec<UtxoEntryReference>`] of the pending transaction
    pub fn utxo_entries(&self) -> &Vec<UtxoEntryReference> {
        &self.inner.utxo_entries
    }

    pub fn fees(&self) -> u64 {
        self.inner.fees
    }

    pub fn input_aggregate_value(&self) -> u64 {
        self.inner.aggregate_input_value
    }

    pub fn output_aggregate_value(&self) -> u64 {
        self.inner.aggregate_output_value
    }

    pub fn payment_value(&self) -> Option<u64> {
        self.inner.payment_value
    }

    pub fn change_value(&self) -> u64 {
        self.inner.change_value
    }

    pub fn is_final(&self) -> bool {
        self.inner.is_final
    }

    pub fn is_batch(&self) -> bool {
        !self.inner.is_final
    }

    async fn commit(&self) -> Result<()> {
        self.inner.is_committed.load(Ordering::SeqCst).then(|| {
            panic!("PendingTransaction::commit() called multiple times");
        });
        self.inner.is_committed.store(true, Ordering::SeqCst);
        if let Some(utxo_context) = self.inner.generator.utxo_context() {
            utxo_context.handle_outgoing_transaction(self).await?;
        }
        Ok(())
    }

    pub fn transaction(&self) -> Transaction {
        self.inner.signable_tx.lock().unwrap().tx.clone()
    }

    pub fn rpc_transaction(&self) -> RpcTransaction {
        self.inner.signable_tx.lock().unwrap().tx.as_ref().into()
    }

    /// Submit the transaction on the supplied rpc
    pub async fn try_submit(&self, rpc: &Arc<DynRpcApi>) -> Result<RpcTransactionId> {
        self.commit().await?; // commit transactions only if we are submitting
        let rpc_transaction: RpcTransaction = self.rpc_transaction();
        Ok(rpc.submit_transaction(rpc_transaction, true).await?)
    }

    pub async fn log(&self) -> Result<()> {
        log_info!("pending transaction: {:?}", self.rpc_transaction());
        Ok(())
    }

    pub fn try_sign(&self) -> Result<()> {
        let signer = self.inner.generator.signer().as_ref().expect("no signer in tx generator");
        let signed_tx = signer.try_sign(self.inner.signable_tx.lock()?.clone(), self.addresses())?;
        *self.inner.signable_tx.lock().unwrap() = signed_tx;
        Ok(())
    }

    pub fn try_sign_with_keys(&self, privkeys: Vec<[u8; 32]>) -> Result<()> {
        let mutable_tx = self.inner.signable_tx.lock()?.clone();
        let signed_tx = sign_with_multiple_v2(mutable_tx, privkeys);
        *self.inner.signable_tx.lock().unwrap() = signed_tx;
        Ok(())
    }
}