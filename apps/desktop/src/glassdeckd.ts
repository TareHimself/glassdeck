import { create } from "zustand";
import { commands, events, type Event, type GlassdeckdError, type GlassdeckdUpdate, type Status } from "./bindings";

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
      return { state: "connected", status: { ...status, link: { ...status.link, connected: event.connected } } };
    case "stream":
      return { state: "connected", status: { ...status, stream: event.status } };
    case "theme":
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

type Store = { connection: Connection };

export const useGlassdeckd = create<Store>(() => ({ connection: { state: "connecting" } }));

export async function followGlassdeckd(): Promise<() => void> {
  const unlisten = await events.glassdeckdUpdate.listen(({ payload }) => {
    useGlassdeckd.setState(({ connection }) => ({ connection: nextConnection(connection, payload) }));
  });
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
