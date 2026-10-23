mod support;

use {
    bitcoin::OutPoint,
    opi_source::driver::spawn_source,
    ord::InscriptionId,
    rand::{Rng, SeedableRng, rngs::StdRng},
    std::{
        io::{BufRead, BufReader},
        path::Path,
        process::{Child, Command, Stdio},
        sync::mpsc::{self, Receiver},
        thread,
        time::{Duration, Instant},
    },
    support::{Harness, dump, envelope, source_config, wait_for_tip},
};

const TIP: u32 = 60;
const CHUNK: u32 = 3;
const REORG_HEIGHT: u32 = 40;
const ITERATIONS: usize = 20;
const DEFAULT_SEED: u64 = 0x0b15_5eed;

fn body(n: u32) -> Vec<u8> {
    format!(r#"{{"p":"brc-20","op":"mint","tick":"ordi","amt":"{n}"}}"#).into_bytes()
}

fn outpoint(id: InscriptionId) -> OutPoint {
    OutPoint {
        txid: id.txid,
        vout: 0,
    }
}

/// Mines to `target`, two blocks per step: a funding block, then a block with
/// a new inscription plus, on some steps, a transfer or a fee spend of an
/// earlier one.
fn extend(harness: &Harness, confirmed: &mut Vec<InscriptionId>, target: u32, step: &mut u32) {
    while harness.height() + 2 <= target {
        *step += 1;
        let id = harness.inscribe(&body(*step), "text/plain");
        match *step % 6 {
            2 | 5 if !confirmed.is_empty() => {
                harness.transfer(outpoint(confirmed.remove(0)), 100);
            }
            4 if !confirmed.is_empty() => {
                harness.spend_to_fee(outpoint(confirmed.remove(0)));
            }
            _ => {}
        }
        harness.mine(1);
        confirmed.push(id);
    }
    let rest = target - harness.height();
    harness.mine(rest as usize);
}

/// A chain to [`TIP`] with creations, transfers and fee spends, whose block
/// `REORG_HEIGHT` replaced an earlier block of the same height.
fn scenario() -> Harness {
    let harness = Harness::new();
    let mut confirmed = Vec::new();
    let mut step = 0;
    extend(&harness, &mut confirmed, REORG_HEIGHT - 2, &mut step);
    assert_eq!(harness.height(), REORG_HEIGHT - 2);

    harness.inscribe(&body(1000), "text/plain");
    harness.mine(1);
    assert_eq!(harness.height(), REORG_HEIGHT);
    harness.invalidate_tip();
    harness.reveal(
        (REORG_HEIGHT - 2) as usize,
        envelope(b"text/plain", &body(2000)),
    );
    harness.mine(1);
    assert_eq!(harness.height(), REORG_HEIGHT);

    extend(&harness, &mut confirmed, TIP, &mut step);
    assert_eq!(harness.height(), TIP);
    harness
}

struct SourceProcess {
    child: Child,
    stdout: Receiver<String>,
    stderr: std::path::PathBuf,
}

impl SourceProcess {
    fn spawn(harness: &Harness, dir: &Path, name: &str) -> SourceProcess {
        let stderr = dir.join(format!("{name}.stderr"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_opi-source-run"))
            .args(["--chain", "regtest", "--chunk", &CHUNK.to_string()])
            .arg("--ord-data-dir")
            .arg(dir.join("ord"))
            .arg("--journal-dir")
            .arg(dir.join("journal"))
            .args(["--bitcoin-rpc-url", &harness.core.url()])
            .args(["--rpc-user", "user", "--rpc-pass", "pass"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, stdout) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        SourceProcess {
            child,
            stdout,
            stderr,
        }
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Fails when the child has already exited.
    fn assert_running(&mut self, context: &str) {
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!(
                "{context}: child exited on its own with {status}\n{}",
                self.stderr()
            );
        }
    }

    /// SIGKILLs the child and reaps it.
    fn kill(mut self, context: &str) {
        self.assert_running(context);
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    /// Waits for the child to report a journal tip of at least `height`.
    fn wait_for_tip(&mut self, height: u32, context: &str) {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match self.stdout.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    let tip = line
                        .strip_prefix("tip ")
                        .and_then(|n| n.parse::<u32>().ok());
                    if tip.is_some_and(|tip| tip >= height) {
                        return;
                    }
                }
                Err(_) => self.assert_running(context),
            }
            assert!(
                Instant::now() < deadline,
                "{context}: tip {height} never reported\n{}",
                self.stderr()
            );
        }
    }

    /// SIGTERMs the child and expects a clean exit.
    fn terminate(mut self, context: &str) {
        self.assert_running(context);
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "{context}: child exited with {status}\n{}",
                    self.stderr()
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{context}: child ignored SIGTERM\n{}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn seed() -> u64 {
    std::env::var("OPI_KILL_SEED")
        .ok()
        .map(|seed| seed.parse().expect("OPI_KILL_SEED is a u64"))
        .unwrap_or(DEFAULT_SEED)
}

#[test]
fn random_kills_converge_to_uninterrupted_journal() {
    let seed = seed();
    println!("OPI_KILL_SEED={seed}");
    let mut rng = StdRng::seed_from_u64(seed);
    let harness = scenario();

    let reference = tempfile::tempdir().unwrap();
    let config = source_config(reference.path(), harness.core.url(), CHUNK);
    let reference_journal = config.journal_path();
    let source = spawn_source(config).unwrap();
    wait_for_tip(&source.reader(), TIP);
    source.shutdown();
    source.join().unwrap();
    let expected = dump(&reference_journal);
    assert!(!expected.is_empty());

    let killed = tempfile::tempdir().unwrap();
    for iteration in 0..ITERATIONS {
        let context = format!("seed {seed}, iteration {iteration}");
        let mut child = SourceProcess::spawn(&harness, killed.path(), &format!("kill-{iteration}"));
        thread::sleep(Duration::from_millis(rng.gen_range(50..=500)));
        child.assert_running(&context);
        child.kill(&context);
    }

    let context = format!("seed {seed}, final run");
    let mut last = SourceProcess::spawn(&harness, killed.path(), "final");
    last.wait_for_tip(TIP, &context);
    last.terminate(&context);

    let actual = dump(&killed.path().join("journal").join("journal.redb"));
    if let Some(at) =
        (0..expected.len().max(actual.len())).find(|&i| expected.get(i) != actual.get(i))
    {
        let describe = |row: Option<&(&str, Vec<u8>, Vec<u8>)>| {
            row.map_or("<missing>".to_string(), |(table, key, _)| {
                format!("{table} {}", hex::encode(key))
            })
        };
        panic!(
            "seed {seed}: journals differ at row {at}: reference {}, killed run {}",
            describe(expected.get(at)),
            describe(actual.get(at)),
        );
    }
}
