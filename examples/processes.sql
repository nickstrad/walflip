-- One row per process; columns mirror `ps -o pid,ppid,pcpu,rss,stat,comm`.
CREATE TABLE processes(
    pid     INTEGER PRIMARY KEY,
    ppid    INTEGER NOT NULL,
    name    TEXT    NOT NULL,
    state   TEXT    NOT NULL,
    cpu_pct REAL    NOT NULL,
    rss_kb  INTEGER NOT NULL
);
