import { create } from "zustand";
import { commands, events, type BackupSummary, type Keyboard, type KeyboardError, type SetupStep, type SetupUpdate } from "./bindings";

export type RowState = "pending" | "running" | "waiting" | "done" | "failed";

/** One line of the Back up section; setup's six steps show as four. */
export type Row = { title: string; steps: SetupStep[]; running: string; done: string };

export const BACKUP_ROWS: Row[] = [
  { title: "Find the key scanner", steps: ["scanner_found"], running: "Asking the key scanner", done: "" },
  { title: "Key scanner driver", steps: ["scanner_driver"], running: "Checking for the driver", done: "Ready" },
  { title: "Back up the key scanner", steps: ["scanner_backup"], running: "Reading flash", done: "Firmware, settings and factory calibration" },
  { title: "Back up the keyboard's computer", steps: ["bridge_found", "probe", "linux_backup"], running: "Copying", done: "Partitions and system files, checked" },
];

/** What a backup run has done so far, built from its `SetupUpdate`s. */
export type Run = {
  running: boolean;
  finished: SetupStep[];
  current: SetupStep | null;
  /** The latest transfer progress, such as "reading flash: 6.00 / 16.00 MiB". */
  progress: string | null;
  needsYou: string[] | null;
  failed: string | null;
  /** The zip this run saved, once it's checked. */
  backedUp: string | null;
};

export const IDLE: Run = { running: false, finished: [], current: null, progress: null, needsYou: null, failed: null, backedUp: null };

export function nextRun(run: Run, update: SetupUpdate): Run {
  switch (update.state) {
    case "started":
      return { ...run, running: true, current: update.step, progress: null, needsYou: null, failed: null };
    case "note":
      return update.progress ? { ...run, progress: update.text } : run;
    case "needs_you":
      return { ...run, needsYou: update.todo };
    case "finished":
      return { ...run, finished: [...run.finished, update.step], current: null, progress: null };
    case "failed":
      return { ...run, failed: update.reason };
    case "backed_up":
      return { ...run, backedUp: update.file };
    case "ended":
      return { ...run, running: false };
  }
}

export function rowState(row: Row, run: Run): RowState {
  if (row.steps.every((s) => run.finished.includes(s))) return "done";
  if (run.current && row.steps.includes(run.current)) {
    if (run.needsYou) return "waiting";
    if (run.failed) return "failed";
    return run.running ? "running" : "pending";
  }
  return "pending";
}

/** "reading flash: 6.00 / 16.00 MiB" as a share and a short amount. */
export function transfer(progress: string | null): { percent: number; amount: string } | null {
  const m = /([\d.]+)\s*\/\s*([\d.]+)\s*(\w+)\s*$/.exec(progress ?? "");
  if (!m) return null;
  const [done, total] = [Number(m[1]), Number(m[2])];
  if (!(total > 0)) return null;
  return { percent: Math.min(100, Math.round((done / total) * 100)), amount: `${done.toFixed(1)} / ${Number(total.toFixed(1))} ${m[3]}` };
}

/** Tauri itself rejects with a plain string (a command it won't run), not one of our errors. */
export function describeKeyboardError(error: KeyboardError | string): string {
  if (typeof error === "string") return error;
  switch (error.kind) {
    case "busy":
      return "A backup is already running";
    case "unavailable":
    case "failed":
      return error.reason;
  }
}

const FOLDER_KEY = "glassdeck.backupsFolder";
/** Backups used to default to Documents; that default now moves to the app's own data. */
const OLD_DEFAULT = /\\Documents\\Glassdeck\\Keyboard backups\\?$/i;

/** The folder the user chose, unless it's only the old default. */
export function chosenFolder(saved: string | null): string | null {
  return saved && !OLD_DEFAULT.test(saved) ? saved : null;
}

type Store = {
  /** null until the first look. */
  keyboard: Keyboard | null;
  /** Why the last look failed, if it did. */
  lookError: string | null;
  folder: string | null;
  backups: BackupSummary[];
  run: Run;
};

export const useKeyboard = create<Store>(() => ({ keyboard: null, lookError: null, folder: null, backups: [], run: IDLE }));

export async function refreshKeyboard() {
  const looked = await commands.keyboard();
  if (looked.status === "ok") useKeyboard.setState({ keyboard: looked.data, lookError: null });
  else useKeyboard.setState({ lookError: describeKeyboardError(looked.error) });
}

export async function refreshBackups() {
  const folder = useKeyboard.getState().folder;
  if (folder) useKeyboard.setState({ backups: await commands.keyboardBackups(folder) });
}

export async function setFolder(folder: string) {
  localStorage.setItem(FOLDER_KEY, folder);
  useKeyboard.setState({ folder });
  await refreshBackups();
}

const LOOK_EVERY_MS = 10_000;

/** Looks at the keyboard now and every few seconds, and follows backup runs. */
export async function followKeyboard(): Promise<() => void> {
  const unlisten = await events.setupUpdate.listen(({ payload }) => {
    useKeyboard.setState(({ run }) => ({ run: nextRun(run, payload) }));
    if (payload.state === "ended") {
      void refreshBackups();
      void refreshKeyboard();
    }
  });
  const saved = chosenFolder(localStorage.getItem(FOLDER_KEY));
  if (saved === null) localStorage.removeItem(FOLDER_KEY);
  const folder = saved ?? (await commands.defaultBackupsFolder().then((r) => (r.status === "ok" ? r.data : null)));
  useKeyboard.setState({ folder });
  void refreshBackups();
  void refreshKeyboard();
  const timer = window.setInterval(() => {
    if (!useKeyboard.getState().run.running) void refreshKeyboard();
  }, LOOK_EVERY_MS);
  return () => {
    unlisten();
    window.clearInterval(timer);
  };
}

/** This keyboard's saved backups, newest first. */
export function mine(store: Store): BackupSummary[] {
  const serial = store.keyboard?.serial;
  return serial ? store.backups.filter((b) => b.serial === serial) : [];
}

/** Whether this keyboard has a saved, checked backup: what Install and Restore wait for. */
export function backedUp(store: Store): boolean {
  return store.run.backedUp !== null || mine(store).length > 0;
}

/** Adds a backup made elsewhere (a zip, or with `folder` a setup folder); why it was refused, or null. */
export async function importBackup(folder: boolean): Promise<string | null> {
  const { folder: store, keyboard } = useKeyboard.getState();
  if (!store) return "Pick where backups go first";
  const picked = await commands.pickBackup(folder);
  if (!picked) return null;
  const result = await commands.importBackup(store, picked, keyboard?.serial ?? null);
  if (result.status === "error") return describeKeyboardError(result.error);
  await refreshBackups();
  return null;
}
