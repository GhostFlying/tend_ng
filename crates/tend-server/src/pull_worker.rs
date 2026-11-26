use crate::{codebase, codebase_comments, github, store::Store};
use protocol::{BranchResult, PullProvider};
use std::{
    ffi::OsString,
    thread,
    time::{Duration, Instant},
};

pub fn spawn(mut store: Store, provider: PullProvider, program: OsString) {
    // A provider that is unavailable or slow must not delay the other's observations.
    thread::spawn(move || {
        loop {
            let result = (|| {
                let Some(pull) = store.due_pull(provider)? else {
                    return Ok(false);
                };
                let observation = match pull.github() {
                    Some(github_pull) => {
                        let watched = store.watched_ci_heads(&github_pull)?;
                        github::observe(&program, &github_pull, &watched)
                    }
                    None => codebase::observe(&program, &pull).map(|facts| (facts, Vec::new())),
                };
                store.observe_pull(&pull, observation)?;
                Ok::<_, anyhow::Error>(true)
            })();
            match result {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => eprintln!("{} pull observation: {error:#}", provider.as_str()),
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}

pub fn spawn_comments(mut store: Store, program: OsString) {
    thread::spawn(move || {
        loop {
            let result = (|| {
                let Some(pull) = store.due_codebase_comments()? else {
                    return Ok(false);
                };
                let started = store.codebase_poll_started(&pull)?;
                let observation = codebase_comments::snapshot(
                    &program,
                    &pull,
                    Instant::now() + codebase::TIMEOUT,
                );
                store.observe_codebase_comments(&pull, &observation, started)?;
                Ok::<_, anyhow::Error>(true)
            })();
            match result {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => eprintln!("Codebase comment observation: {error:#}"),
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}

pub fn spawn_discovery(
    mut store: Store,
    worker: PullProvider,
    github_cli: OsString,
    codebase_cli: OsString,
) {
    thread::spawn(move || {
        loop {
            let outcome = (|| {
                let Some(request) = store.pending_discovery(worker)? else {
                    return Ok(false);
                };
                let results: Vec<_> = request
                    .branches
                    .iter()
                    .map(|branch| {
                        let result = match branch.provider {
                            PullProvider::GitHub => {
                                github::find(&github_cli, branch).map(|pull| pull.map(|p| p.url()))
                            }
                            PullProvider::Codebase => codebase::find(&codebase_cli, branch)
                                .map(|pull| pull.map(|p| p.url())),
                        };
                        let (state, url) = match result {
                            Ok(Some(url)) => ("linked", Some(url)),
                            Ok(None) => ("missing", None),
                            Err(reason) => (reason, None),
                        };
                        BranchResult {
                            branch: branch.clone(),
                            state: state.to_owned(),
                            url,
                        }
                    })
                    .collect();
                store.finish_discovery(&request, &results, &codebase_cli)?;
                Ok::<_, anyhow::Error>(true)
            })();
            match outcome {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => eprintln!("{} pull branch discovery: {error:#}", worker.as_str()),
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}
