//! Scenario runner (docs/TESTING_PLAN.md section 3): a scenario is plain data, an ordered list
//! of [`Step`]s, executed by a [`Driver`] (the compiled binary or the `walflip::db` library).
//! After every step the runner syncs the [`Model`] and calls `assert_invariants`.
//!
//! ```ignore
//! let s = Scenario::new("s", "CREATE TABLE t(id INTEGER PRIMARY KEY);", "d", vec![
//!     Step::Exec("INSERT INTO t DEFAULT VALUES".into()),
//!     Step::Clone(1),
//! ]);
//! assert_same_outcome(&s.run(Driver::Cli), &s.run(Driver::Lib));
//! ```

use super::invariants::{assert_invariants, Checks, LogRow, Observed};
use super::model::Model;
use super::{parse_captured, Harness};
use std::fs;
use std::path::Path;
use walflip::db::{self, Db};

/// One scenario action. The db it acts on is the scenario's current name (see `Rename`).
#[derive(Clone, Debug)]
pub enum Step {
    /// One statement via `exec`; must succeed. The model is fed the same text.
    Exec(String),
    /// Several statements via one stdin batch (`exec -`) / one `db::exec` call; must succeed.
    /// Statements must be single-line and must each change contents (see `Model::exec_batch`).
    Batch(Vec<String>),
    /// A statement that must be rejected; state and model stay unchanged (INV-37).
    ExecFails(String),
    /// `clone -n k` to a fresh path; bytes are recorded in `Outcome::clones`.
    Clone(u64),
    /// Rename the db; later steps and checks use the new name (INV-43).
    Rename(String),
    /// Drop the db; the folder must be gone. Must be the last step (INV-43).
    Drop,
}

/// Which implementation executes the steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Driver {
    Cli,
    Lib,
}

pub struct Scenario {
    pub name: &'static str,
    pub schema: &'static str,
    /// Name the db is created under.
    pub db: &'static str,
    pub steps: Vec<Step>,
    pub checks: Checks,
}

/// What a run produced, comparable across drivers.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    /// `Observed` after create and after each step (`None` after `Drop`).
    pub observed: Vec<Option<Observed>>,
    /// (k, bytes) for every `Step::Clone`, in order.
    pub clones: Vec<(u64, Vec<u8>)>,
}

impl Outcome {
    /// Last observed log (empty if the db was dropped last and nothing else was observed).
    pub fn log(&self) -> Vec<LogRow> {
        self.observed
            .iter()
            .rev()
            .flatten()
            .next()
            .map(|o| o.log.clone())
            .unwrap_or_default()
    }
}

impl Scenario {
    pub fn new(
        name: &'static str,
        schema: &'static str,
        db: &'static str,
        steps: Vec<Step>,
    ) -> Self {
        Self {
            name,
            schema,
            db,
            steps,
            checks: Checks::full(),
        }
    }

    pub fn checks(mut self, c: Checks) -> Self {
        self.checks = c;
        self
    }

    /// Run all steps through `driver` in a fresh temp dir, checking invariants after each.
    pub fn run(&self, driver: Driver) -> Outcome {
        let h = Harness::new();
        let root = h.root();
        let who = |step: usize| format!("{} [{driver:?}] step {step}", self.name);
        let mut cur = self.db.to_string();
        let mut model = Model::new(Some(self.schema));
        let mut out = Outcome {
            observed: vec![],
            clones: vec![],
        };

        match driver {
            Driver::Cli => {
                h.create(&cur, self.schema);
            }
            Driver::Lib => {
                db::create(&Db::new(&root, &cur).unwrap(), Some(self.schema))
                    .unwrap_or_else(|e| panic!("{}: create: {e:#}", who(0)));
            }
        }
        out.observed.push(Some(assert_invariants(
            &root,
            &cur,
            Some(&model),
            self.checks,
        )));

        for (i, step) in self.steps.iter().enumerate() {
            let i = i + 1;
            let mut dropped = false;
            match step {
                Step::Exec(sql) => {
                    let n = match driver {
                        Driver::Cli => h.exec(&cur, sql),
                        Driver::Lib => lib_exec(&root, &cur, std::slice::from_ref(sql))
                            .unwrap_or_else(|e| panic!("{}: {e}", who(i))),
                    };
                    model.exec(sql, n);
                }
                Step::Batch(stmts) => {
                    let n = match driver {
                        Driver::Cli => {
                            let o = h.with_stdin(&["--db", &cur, "exec", "-"], &stmts.join("\n"));
                            assert!(
                                o.status.success(),
                                "{}: {}",
                                who(i),
                                String::from_utf8_lossy(&o.stderr)
                            );
                            parse_captured(&String::from_utf8(o.stdout).unwrap())
                        }
                        Driver::Lib => lib_exec(&root, &cur, stmts)
                            .unwrap_or_else(|e| panic!("{}: {e}", who(i))),
                    };
                    model.exec_batch(stmts, n);
                }
                Step::ExecFails(sql) => {
                    let failed = match driver {
                        Driver::Cli => !h.run(&["--db", &cur, "exec", sql]).status.success(),
                        Driver::Lib => lib_exec(&root, &cur, std::slice::from_ref(sql)).is_err(),
                    };
                    assert!(
                        failed,
                        "INV-37: {}: statement unexpectedly succeeded: {sql}",
                        who(i)
                    );
                    model.failed(sql);
                }
                Step::Clone(k) => {
                    let path = h.path(&format!("step{i}-clone{k}.db"));
                    match driver {
                        Driver::Cli => {
                            h.ok(&[
                                "--db",
                                &cur,
                                "clone",
                                "-n",
                                &k.to_string(),
                                "-o",
                                path.to_str().unwrap(),
                            ]);
                        }
                        Driver::Lib => {
                            db::clone_at(&Db::existing(&root, &cur).unwrap(), *k, &path)
                                .unwrap_or_else(|e| panic!("INV-29: {}: {e:#}", who(i)));
                        }
                    }
                    out.clones.push((*k, fs::read(&path).unwrap()));
                }
                Step::Rename(to) => {
                    match driver {
                        Driver::Cli => {
                            h.ok(&["rename", &cur, to]);
                        }
                        Driver::Lib => db::rename(&root, &cur, to)
                            .unwrap_or_else(|e| panic!("{}: {e:#}", who(i))),
                    }
                    assert!(
                        !root.join(&cur).exists(),
                        "INV-43: {}: old folder remains",
                        who(i)
                    );
                    cur = to.clone();
                }
                Step::Drop => {
                    match driver {
                        Driver::Cli => {
                            h.ok(&["drop", &cur]);
                        }
                        Driver::Lib => db::drop_db(&Db::existing(&root, &cur).unwrap()).unwrap(),
                    }
                    assert!(
                        !root.join(&cur).exists(),
                        "INV-43: {}: drop left the folder",
                        who(i)
                    );
                    assert!(
                        i == self.steps.len(),
                        "{}: Drop must be the last step",
                        who(i)
                    );
                    dropped = true;
                }
            }
            out.observed.push(if dropped {
                None
            } else {
                Some(assert_invariants(&root, &cur, Some(&model), self.checks))
            });
        }
        out
    }
}

fn lib_exec(root: &Path, name: &str, stmts: &[String]) -> Result<u64, String> {
    let d = Db::existing(root, name).map_err(|e| format!("{e:#}"))?;
    db::exec(&d, stmts)
        .map(|n| n as u64)
        .map_err(|e| format!("{e:#}"))
}

/// INV-44: two drivers given the same steps must agree on every observed log and clone byte.
pub fn assert_same_outcome(a: &Outcome, b: &Outcome) {
    assert_eq!(
        a.observed.len(),
        b.observed.len(),
        "INV-44: step counts differ"
    );
    for (i, (x, y)) in a.observed.iter().zip(&b.observed).enumerate() {
        assert_eq!(x, y, "INV-44: observed state differs after step {i}");
    }
    assert_eq!(
        a.clones.len(),
        b.clones.len(),
        "INV-44: clone counts differ"
    );
    for ((ka, ba), (kb, bb)) in a.clones.iter().zip(&b.clones) {
        assert_eq!(ka, kb);
        assert!(
            ba == bb,
            "INV-44: clone -n {ka} bytes differ between drivers ({} vs {})",
            ba.len(),
            bb.len()
        );
    }
}
