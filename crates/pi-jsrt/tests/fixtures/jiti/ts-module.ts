// Fixture 3 (TS): TypeScript syntax, importing ESM and CJS siblings.
import greet, { kind as esmKind } from "./esm.mjs";
import * as cjs from "./cjs.cjs";
import type { Unused } from "./types";

export enum Mode {
  Slow = 1,
  Fast,
}

class Counter {
  constructor(private value: number) {}
  next(): number {
    return ++this.value;
  }
}

const counter = new Counter(0);
export const state = { evaluations: counter.next() };
export const summary: Record<string, unknown> = {
  greeting: greet("ts"),
  esmKind,
  cjsKind: cjs.kind,
  mode: Mode.Fast,
};
export type { Unused };
