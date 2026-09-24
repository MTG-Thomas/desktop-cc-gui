import type { SshHost } from "@/lib/ipc";

/** Port field: integers 1-65535, otherwise null (never NaN into settings). */
export function parsePort(raw: string): number | null {
  const trimmed = raw.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const port = Number(trimmed);
  return port >= 1 && port <= 65535 ? port : null;
}

/** Compact engine list for a host row: "muse, codex (+2 more)". */
export function enginesSummary(engines: Record<string, string>): string {
  const names = Object.keys(engines).sort();
  if (names.length === 0) return "";
  const head = names.slice(0, 2).join(", ");
  return names.length > 2 ? `${head} (+${names.length - 2} more)` : head;
}

/** Fresh enrolled-host record; probe fills engines/lastOk after. */
export function newSshHost(host: string, user: string, port: number): SshHost {
  return {
    id: crypto.randomUUID(),
    host: host.trim(),
    user: user.trim(),
    port,
    engines: {},
    lastOk: false,
    lastProbe: null,
  };
}

/** Config candidates not already enrolled (matched by alias). */
export function filterNewCandidates<T extends { alias: string }, H extends { host: string }>(
  candidates: T[],
  enrolled: H[],
): T[] {
  const known = new Set(enrolled.map((h) => h.host));
  return candidates.filter((c) => !known.has(c.alias));
}
