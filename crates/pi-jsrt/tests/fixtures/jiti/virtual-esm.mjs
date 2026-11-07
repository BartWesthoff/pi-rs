// A native ES module that imports the virtual module: its native import cannot link, so jiti
// falls back to loading it itself (with the virtual module).
import { createAgentSession } from "@earendil-works/pi-coding-agent";
export const fromEsm = createAgentSession;
