import { useEffect, useState, type ReactNode } from "react";
import { commands, type BackgroundSummary, type BackupSummary, type Keyboard as KeyboardInfo } from "../bindings";
import { Button } from "../components/Button";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { keyboardLabel, useGlassdeckd } from "../glassdeckd";
import { BACKUP_ROWS, backedUp, describeKeyboardError, importBackup, mine, rowState, setFolder, transfer, useKeyboard, type Row, type RowState, type Run } from "../keyboard";
import { BUILT_IN } from "../library";
import styles from "./Keyboard.module.css";

const INSTALL_ROWS = ["Glassdeck on the key scanner", "Glassdeck on the keyboard's computer", "Restart the keyboard"];
const ZADIG_URL = "https://zadig.akeo.ie";

function StepIcon({ state }: { state: RowState }) {
  switch (state) {
    case "done":
      return (
        <svg width="22" height="22" viewBox="0 0 22 22" fill="none">
          <circle cx="11" cy="11" r="10" fill="#173826" />
          <path d="M6.5 11.2l3 3 6-6.4" stroke="#3ecf8e" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      );
    case "running":
      return (
        <svg className={styles.spin} width="20" height="20" viewBox="0 0 20 20" fill="none">
          <circle cx="10" cy="10" r="8" stroke="#2f3643" strokeWidth="2.4" />
          <path d="M10 2a8 8 0 0 1 8 8" stroke="#6aa8ff" strokeWidth="2.4" strokeLinecap="round" />
        </svg>
      );
    case "waiting":
    case "failed":
      return (
        <svg width="22" height="22" viewBox="0 0 22 22" fill="none">
          <circle cx="11" cy="11" r="10" fill={state === "waiting" ? "#3a2c12" : "#3a1616"} />
          <path d="M11 6.5v6M11 15.5v.2" stroke={state === "waiting" ? "#f5b84b" : "#ff8a7a"} strokeWidth="2.2" strokeLinecap="round" />
        </svg>
      );
    case "pending":
      return (
        <svg width="20" height="20" viewBox="0 0 20 20" fill="none">
          <circle cx="10" cy="10" r="8" stroke="#2f3643" strokeWidth="2" />
        </svg>
      );
  }
}

function StepLine({ title, state, detail, amount, percent, children }: { title: string; state: RowState; detail: string; amount?: string; percent?: number; children?: ReactNode }) {
  return (
    <li className={styles.step} data-active={state === "pending" ? undefined : true}>
      <div className={styles.stepLine}>
        <span className={styles.icon} aria-hidden="true">
          <StepIcon state={state} />
        </span>
        <div className={styles.stepText}>
          <div className={styles.stepTitle}>{title}</div>
          {detail && <div className={styles.stepDetail}>{detail}</div>}
        </div>
        {amount && <span className={styles.amount}>{amount}</span>}
      </div>
      {percent !== undefined && (
        <div className={styles.bar} role="progressbar" aria-label={title} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}>
          <div className={styles.barFill} style={{ width: `${percent}%` }} />
        </div>
      )}
      {children}
    </li>
  );
}

function rowDetail(row: Row, state: RowState, run: Run, keyboard: KeyboardInfo): string {
  switch (state) {
    case "done":
      return row.steps[0] === "scanner_found" ? `Flux firmware ${keyboard.firmware ?? ""}`.trim() : row.done;
    case "running":
      return row.running;
    case "waiting":
      return "Driver missing";
    case "failed":
      return run.failed ?? "";
    case "pending":
      return "";
  }
}

function BackupRow({ row, run, keyboard }: { row: Row; run: Run; keyboard: KeyboardInfo }) {
  const state = rowState(row, run);
  const moving = state === "running" ? transfer(run.progress) : null;
  return (
    <StepLine title={row.title} state={state} detail={rowDetail(row, state, run, keyboard)} amount={moving?.amount} percent={moving?.percent}>
      {state === "waiting" && (
        <div className={styles.needsYou}>
          <div className={styles.needsYouTitle}>Windows needs a driver to read the key scanner's memory</div>
          <ol className={styles.todo}>
            <li>Open Zadig and turn on Options › List All Devices</li>
            <li>
              Pick <strong>RP2 Boot (Interface 1)</strong>, driver <strong>WinUSB</strong>, then Install Driver
            </li>
            <li>Come back here</li>
          </ol>
          <div className={styles.needsYouActions}>
            <button type="button" className={styles.retry} onClick={() => void startBackup()}>
              Try again
            </button>
            <a className={styles.zadig} href={ZADIG_URL} target="_blank" rel="noreferrer">
              Get Zadig
            </a>
          </div>
        </div>
      )}
    </StepLine>
  );
}

async function startBackup() {
  const { folder } = useKeyboard.getState();
  if (!folder) return;
  const started = await commands.backUpKeyboard(folder);
  if (started.status === "error") useKeyboard.setState(({ run }) => ({ run: { ...run, failed: describeKeyboardError(started.error) } }));
}

function SetUp({ keyboard }: { keyboard: KeyboardInfo }) {
  const store = useKeyboard();
  const { folder, run } = store;
  const [installProblem, setInstallProblem] = useState<string | null>(null);
  const done = backedUp(store);
  const change = async () => {
    const picked = await commands.pickBackupsFolder();
    if (picked) await setFolder(picked);
  };
  const install = async () => {
    const result = await commands.installGlassdeck();
    setInstallProblem(result.status === "error" ? describeKeyboardError(result.error) : null);
  };
  return (
    <>
      <section aria-label="Where backups go" className={styles.folder}>
        <label htmlFor="backups-folder" className={styles.folderLabel}>
          Save backups to
        </label>
        <input id="backups-folder" className={styles.folderPath} type="text" value={folder ?? ""} readOnly />
        <Button className={styles.small} onClick={() => void change()} disabled={run.running}>
          Change
        </Button>
      </section>

      <section aria-label="Back up" className={styles.section}>
        <div className={styles.sectionHead}>
          <h2 className={styles.sectionTitle}>Back up</h2>
          <Button variant={done ? "secondary" : "primary"} disabled={run.running || done || !folder} onClick={() => void startBackup()}>
            {run.running ? "Backing up…" : done ? "Backed up" : "Back up"}
          </Button>
        </div>
        <ol className={styles.steps}>
          {BACKUP_ROWS.map((row) => (
            <BackupRow key={row.title} row={row} run={run} keyboard={keyboard} />
          ))}
        </ol>
        {run.failed && !run.current && <Problem>{run.failed}</Problem>}
        <Backups />
      </section>

      <section aria-label="Install" className={styles.section} data-locked={done ? undefined : true}>
        <div className={styles.sectionHead}>
          <h2 className={styles.sectionTitle}>Install</h2>
          <Button variant="primary" disabled={!done || run.running} onClick={() => void install()}>
            Install Glassdeck
          </Button>
        </div>
        {!done && <div className={styles.hint}>Back up first</div>}
        <ol className={styles.steps}>
          {INSTALL_ROWS.map((title) => (
            <StepLine key={title} title={title} state="pending" detail="" />
          ))}
        </ol>
        {installProblem && <Problem>{installProblem}</Problem>}
      </section>
    </>
  );
}

function Facts({ title, facts }: { title: string; facts: [string, string][] }) {
  return (
    <section aria-label={title} className={styles.card}>
      <h2 className={styles.cardTitle}>{title}</h2>
      <div className={styles.facts}>
        {facts.map(([name, value]) => (
          <div key={name} style={{ display: "contents" }}>
            <span className={styles.factName}>{name}</span>
            <span className={styles.factValue}>{value}</span>
          </div>
        ))}
      </div>
    </section>
  );
}

const GIB = 1024 ** 3;

function backupDate(backup: BackupSummary): string {
  return new Date(backup.created * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

function backupTime(backup: BackupSummary): string {
  return new Date(backup.created * 1000).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
}

const MB = 1000 ** 2;

/** This keyboard's backups: one row each, to show or export, and importing one made elsewhere. */
function Backups() {
  const store = useKeyboard();
  const backups = mine(store);
  const [problem, setProblem] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const bring = async (folder: boolean) => {
    setImporting(true);
    setProblem(await importBackup(folder));
    setImporting(false);
  };
  const act = async (done: Promise<{ status: "ok" } | { status: "error"; error: Parameters<typeof describeKeyboardError>[0] }>) => {
    const result = await done;
    setProblem(result.status === "error" ? describeKeyboardError(result.error) : null);
  };
  return (
    <div className={styles.backups}>
      <div className={styles.cardHead}>
        <h2 className={styles.cardTitle}>Backups</h2>
        <div className={styles.backupActions}>
          <Button className={styles.small} disabled={importing} onClick={() => void bring(false)}>
            Import zip
          </Button>
          <Button className={styles.small} disabled={importing} onClick={() => void bring(true)}>
            Import folder
          </Button>
        </div>
      </div>
      {backups.length === 0 && <div className={styles.hint}>None yet</div>}
      {backups.map((b) => (
        <div key={b.file} className={styles.backup}>
          <span className={styles.backupWhen}>
            {backupDate(b)}, {backupTime(b)}
          </span>
          <span className={styles.backupWhat}>{[`Flux firmware ${b.firmware}`, b.display && `display ${b.display}`, `${Math.round(b.bytes / MB)} MB`].filter(Boolean).join(", ")}</span>
          <div className={styles.backupActions}>
            <Button className={styles.small} onClick={() => void act(commands.revealBackup(b.file))}>
              Show in folder
            </Button>
            <Button className={styles.small} onClick={() => void act(commands.exportBackup(b.file))}>
              Export
            </Button>
          </div>
        </div>
      ))}
      {backups.length > 0 && <div className={styles.hint}>Keep a copy off this PC. The factory calibration can't be made again.</div>}
      {problem && <Problem>{problem}</Problem>}
    </div>
  );
}

function Installed({ keyboard }: { keyboard: KeyboardInfo }) {
  const store = useKeyboard();
  const connection = useGlassdeckd((s) => s.connection);
  const theme = useGlassdeckd((s) => s.theme);
  const [backgrounds, setBackgrounds] = useState<BackgroundSummary[]>([BUILT_IN]);
  useEffect(() => {
    void commands.listBackgrounds().then((r) => r.status === "ok" && setBackgrounds([BUILT_IN, ...r.data]));
  }, []);
  const [confirming, setConfirming] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const newest = mine(store)[0];
  const computer = keyboard.computer;
  const background = theme ? (backgrounds.find((b) => b.id === theme.background.package)?.title ?? null) : null;
  const linkFacts: [string, string][] = [["Connection", connection.state === "connected" ? keyboardLabel(connection.status.link.connection) : "No glassdeckd"]];
  if (background) linkFacts.push(["Background", background]);
  if (theme) linkFacts.push(["Widgets", String(theme.widgets?.length ?? 0)]);
  const restore = async (backup: BackupSummary) => {
    setConfirming(false);
    const result = await commands.restoreFluxSoftware(backup.file);
    setProblem(result.status === "error" ? describeKeyboardError(result.error) : null);
  };
  return (
    <>
      <div className={styles.cards}>
        <Facts title="Key scanner" facts={[["Firmware", keyboard.firmware ?? "Not answering"]]} />
        {computer && (
          <Facts
            title="Keyboard's computer"
            facts={[
              ["System", `Flux OS ${computer.os}`],
              ["Free space", `${(computer.free_bytes / GIB).toFixed(1)} GB`],
            ]}
          />
        )}
        <Facts title="Link" facts={linkFacts} />
      </div>

      <section aria-label="Backups" className={styles.card}>
        <Backups />
      </section>

      <section aria-label="Restore Flux software" className={styles.card}>
        <div className={styles.restoreHead}>
          <div className={styles.restoreText}>
            <h2 className={styles.restoreTitle}>Restore Flux software</h2>
            <div className={styles.hint}>{newest ? `From the backup of ${backupDate(newest)}` : "Back up first"}</div>
          </div>
          {!newest ? (
            <button type="button" className={styles.danger} disabled aria-disabled="true">
              Restore
            </button>
          ) : confirming ? (
            <>
              <button type="button" className={styles.cancel} onClick={() => setConfirming(false)}>
                Cancel
              </button>
              <button type="button" className={styles.dangerNow} onClick={() => void restore(newest)}>
                Restore now
              </button>
            </>
          ) : (
            <button type="button" className={styles.danger} onClick={() => setConfirming(true)}>
              Restore
            </button>
          )}
        </div>
        {problem && <Problem>{problem}</Problem>}
      </section>
    </>
  );
}

export function Keyboard() {
  const keyboard = useKeyboard((s) => s.keyboard);
  const lookError = useKeyboard((s) => s.lookError);
  return (
    <div className={styles.page}>
      <PageHeader title="Keyboard" id={keyboard?.serial ? `Serial ${keyboard.serial}` : undefined} />
      {keyboard === null || keyboard.software === "absent" ? (
        <div className={styles.hint}>{keyboard === null ? (lookError ?? "Looking for the keyboard…") : "No keyboard found. Plug it into this PC."}</div>
      ) : keyboard.software === "flux" ? (
        <SetUp keyboard={keyboard} />
      ) : (
        <Installed keyboard={keyboard} />
      )}
    </div>
  );
}
