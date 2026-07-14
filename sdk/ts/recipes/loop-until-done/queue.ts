// A file-backed work queue so the kickoff process and each loop run share durable state
// (the spec's illustrative `./queue`). The file path comes from ORCR_RECIPE_QUEUE_FILE.
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import path from "node:path";

function file(): string {
  return process.env.ORCR_RECIPE_QUEUE_FILE ?? path.join(process.cwd(), "queue.json");
}
function load(): string[] {
  if (!existsSync(file())) return [];
  try {
    return JSON.parse(readFileSync(file(), "utf8")) as string[];
  } catch {
    return [];
  }
}
function save(items: string[]): void {
  writeFileSync(file(), JSON.stringify(items));
}

export function seedQueue(items: string[]): void {
  save(items);
}
export function queueSize(): number {
  return load().length;
}
export async function workOneItem(): Promise<string | undefined> {
  const items = load();
  const item = items.shift();
  save(items);
  return item;
}
