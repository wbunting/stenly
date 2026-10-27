// Reads Stenly's dictation history (`<data dir>/dictation/history.jsonl`).
// The file is append-only JSON lines, oldest first; see
// `crates/stenly-core/src/dictation.rs` for the writer.

import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export interface Dictation {
  /** RFC 3339 timestamp with offset. */
  at: string;
  /** Seconds of audio. */
  duration: number;
  /** Text after the local pipeline. */
  text: string;
  /** AI-cleaned text, when that ran. */
  enhanced?: string;
  /** App that had focus while dictating. */
  app?: string;
}

/** What Stenly pasted (or would have). */
export function output(d: Dictation): string {
  return d.enhanced ?? d.text;
}

function expandTilde(p: string): string {
  return p === "~" || p.startsWith("~/") ? join(homedir(), p.slice(1)) : p;
}

/** Mirrors `stenly_core::config::data_dir`. */
export function dataDir(override?: string): string {
  const custom = override?.trim() || process.env.STENLY_DATA_DIR;
  if (custom) return expandTilde(custom);
  if (process.platform === "darwin") return join(homedir(), "Library", "Application Support", "stenly");
  const xdg = process.env.XDG_DATA_HOME;
  return join(xdg || join(homedir(), ".local", "share"), "stenly");
}

export function historyFile(override?: string): string {
  return join(dataDir(override), "dictation", "history.jsonl");
}

/** Validate one parsed JSON value into a Dictation, or null if it isn't one. */
export function toDictation(v: unknown): Dictation | null {
  if (!v || typeof v !== "object") return null;
  const r = v as Record<string, unknown>;
  if (typeof r.text !== "string" || typeof r.at !== "string") return null;
  if (Number.isNaN(new Date(r.at).getTime())) return null;
  const d: Dictation = {
    at: r.at,
    duration: typeof r.duration === "number" && Number.isFinite(r.duration) ? r.duration : 0,
    text: r.text,
  };
  if (typeof r.enhanced === "string") d.enhanced = r.enhanced;
  if (typeof r.app === "string" && r.app) d.app = r.app;
  return d;
}

/** Parse history file contents, newest first. Malformed lines are skipped. */
export function parseHistory(contents: string): Dictation[] {
  const out: Dictation[] = [];
  for (const line of contents.split("\n")) {
    if (!line.trim()) continue;
    try {
      const d = toDictation(JSON.parse(line));
      if (d) out.push(d);
    } catch {
      // skip
    }
  }
  return out.reverse();
}

/**
 * Escape dictated text so the detail pane renders it as plain text. Without
 * this, text like `![x](https://…)` or `<img src=…>` would make the launcher
 * fetch a remote URL when the dictation is shown.
 */
export function escapeMarkdown(s: string): string {
  return s
    .replace(/[\\`*_{}[\]()#+\-.!|<>~&]/g, (c) => `\\${c}`)
    .replace(/\r?\n/g, "  \n");
}

/** Newest first; empty if Stenly has no history yet. */
export function readHistory(override?: string): Dictation[] {
  try {
    return parseHistory(readFileSync(historyFile(override), "utf8"));
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw e;
  }
}
