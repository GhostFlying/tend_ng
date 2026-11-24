use crate::{Config, Server, Store};
use anyhow::Result;
use std::{
    sync::{
        Arc,
        mpsc::{self, SyncSender},
    },
    thread,
};

pub fn start(config: Arc<Config>, mut store: Store, server: Arc<Server>) -> SyncSender<()> {
    let (send, receive) = mpsc::sync_channel(0);
    thread::spawn(move || {
        while receive.recv().is_ok() {
            match store.reconciliation_tasks() {
                Ok(tasks) => {
                    for id in tasks {
                        let path = protocol::reconcile_path(&config.runner_id, &id);
                        if let Err(error) = task(&config, &mut store, &server, &id, &path) {
                            eprintln!("Reconcile Task {id}: {error:#}");
                        }
                    }
                }
                Err(error) => eprintln!("List Tasks for reconciliation: {error:#}"),
            }
        }
    });
    send
}

pub fn task(
    config: &Config,
    store: &mut Store,
    server: &Server,
    id: &str,
    path: &str,
) -> Result<()> {
    let (receipts_end, records_end) = store.reconcile_bounds(id)?;
    let mut after = 0;
    while after < receipts_end {
        let page = store.reconcile_receipts(id, after, receipts_end)?;
        let Some(&(last, _)) = page.last() else { break };
        server.exchange(
            config,
            store,
            path,
            page.into_iter().map(|(_, receipt)| receipt).collect(),
            Vec::new(),
        )?;
        after = last;
    }
    let mut after = 0;
    while after < records_end {
        let page = store.reconcile_records(id, after, records_end)?;
        let Some(&(last, _)) = page.last() else { break };
        server.exchange(
            config,
            store,
            path,
            Vec::new(),
            page.into_iter().map(|(_, record)| record).collect(),
        )?;
        after = last;
    }
    Ok(())
}
