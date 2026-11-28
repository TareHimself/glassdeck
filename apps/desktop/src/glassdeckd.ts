import { create } from "zustand";
import { commands, events, type Event, type GlassdeckdError, type GlassdeckdUpdate, type KeyboardLink, type Status, type StreamEntry, type Theme } from "./bindings";

export type Connection =
  | { state: "connecting" }
  | { state: "syncing" }
  | { state: "connected"; status: Status }
  | { state: "lost"; error: GlassdeckdError };

export function nextConnection(current: Connection, update: GlassdeckdUpdate): Connection {
  switch (update.state) {
    case "connected":
      return { state: "syncing" };
    case "lost":
      return { state: "lost", error: update.error };
    case "message":
      return applyEvent(current, update.event);
  }
}

function applyEvent(current: Connection, event: Event): Connection {
  if (event.type === "status") {
    return { state: "connected", status: event.status };
  }
  if (current.state !== "connected") {
    return current;
  }
  const status = current.status;
  switch (event.type) {
    case "link":
      return { state: "connected", status: { ...status, link: { ...status.link, connection: event.connection } } };
    case "stream":
      return { state: "connected", status: { ...status, stream: event.status } };
    case "theme":
    case "streams":
      return current;
  }
}

export function describeError(error: GlassdeckdError): string {
  switch (error.kind) {
    case "not_running":
      return "glassdeckd isn't running";
    case "unreachable":
      return `glassdeckd didn't answer: ${error.reason}`;
    case "refused":
      return error.error.error;
    case "bad_answer":
      return `glassdeckd sent something unexpected: ${error.reason}`;
  }
}

/** Why the keyboard can't be used, in words that say what to do; null when it's connected. */
export function keyboardProblem(link: KeyboardLink): string | null {
  switch (link.state) {
    case "connected":
      return null;
    case "unreachable":
      return "The keyboard isn't answering. Check that it's on and on the same network as this PC.";
    case "incompatible":
      return link.keyboard < link.host
        ? "The keyboard's Glassdeck software is older than this PC's. Update the keyboard, then apply again."
        : "This PC's Glassdeck is older than the keyboard's. Update it, then apply again.";
  }
}

/** The sidebar's short name for the keyboard link. */
export function keyboardLabel(link: KeyboardLink): string {
  switch (link.state) {
    case "connected":
      return "Keyboard connected";
    case "unreachable":
      return "Keyboard not connected";
    case "incompatible":
      return link.keyboard < link.host ? "Keyboard needs an update" : "Glassdeck needs an update";
  }
}

type Store = { connection: Connection; theme: Theme | null; streams: StreamEntry[] };

export const useGlassdeckd = create<Store>(() => ({ connection: { state: "connecting" }, theme: null, streams: [] }));

export async function refreshStreams() {
  const streams = await commands.streams();
  if (streams.status === "ok") useGlassdeckd.setState({ streams: streams.data.streams });
}

async function refreshTheme() {
  const theme = await commands.theme();
  useGlassdeckd.setState({ theme: theme.status === "ok" ? theme.data : null });
}

export async function followGlassdeckd(): Promise<() => void> {
  const unlisten = await events.glassdeckdUpdate.listen(({ payload }) => {
    useGlassdeckd.setState(({ connection }) => ({ connection: nextConnection(connection, payload) }));
    if (payload.state === "message" && payload.event.type === "theme") {
      useGlassdeckd.setState({ theme: payload.event.theme });
    } else if (payload.state === "message" && payload.event.type === "streams") {
      useGlassdeckd.setState({ streams: payload.event.streams.streams });
    } else if (payload.state === "message" && payload.event.type === "status") {
      void refreshTheme();
    }
  });
  void refreshTheme();
  const first = await commands.status();
  if (first.status === "ok") {
    useGlassdeckd.setState(({ connection }) =>
      connection.state === "connecting" ? { connection: { state: "connected", status: first.data } } : { connection },
    );
  } else {
    useGlassdeckd.setState({ connection: { state: "lost", error: first.error } });
  }
  return unlisten;
}
