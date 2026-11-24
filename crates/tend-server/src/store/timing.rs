use rusqlite::{Connection, Result, Transaction, TransactionBehavior};
use std::{env, time::Instant};

pub struct WriteTiming {
    operation: &'static str,
    requested: Instant,
    acquired: Instant,
}

pub fn begin<'db>(
    db: &'db mut Connection,
    operation: &'static str,
) -> Result<(Transaction<'db>, Option<WriteTiming>)> {
    let requested =
        (env::var_os("TEND_SQLITE_TIMING").as_deref() == Some("1".as_ref())).then(Instant::now);
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let timing = requested.map(|requested| WriteTiming {
        operation,
        requested,
        acquired: Instant::now(),
    });
    Ok((tx, timing))
}

pub fn commit(tx: Transaction<'_>, timing: Option<WriteTiming>) -> Result<()> {
    let Some(timing) = timing else {
        return tx.commit();
    };
    let mutated = Instant::now();
    tx.commit()?;
    eprintln!(
        "sqlite-write operation={} wait_us={} transaction_us={} commit_us={}",
        timing.operation,
        timing.acquired.duration_since(timing.requested).as_micros(),
        mutated.duration_since(timing.acquired).as_micros(),
        mutated.elapsed().as_micros(),
    );
    Ok(())
}
