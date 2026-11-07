// Fixture 6 (virtual): imports a virtual module that exists nowhere on disk.
import * as sdk from "@earendil-works/pi-coding-agent";
import sdkDefault, { createAgentSession, VERSION } from "@earendil-works/pi-coding-agent";
import { fromEsm } from "./virtual-esm.mjs";

const host = (globalThis as any).__jitiFixtureSdk;
export const identity = {
  named: createAgentSession === host.createAgentSession,
  defaultImport: sdkDefault === host,
  namespaceProperty: sdk.createAgentSession === host.createAgentSession,
  viaNativeFallback: fromEsm === host.createAgentSession,
  version: VERSION,
};
