// Fixture 4 (default interop): a default export object whose properties jiti's interop proxy
// exposes, next to a callable default.
import legacy from "./legacy.cjs";

export default {
  label: "default-object",
  shout(text: string): string {
    return text.toUpperCase();
  },
};
export const legacyLabel = legacy.label;
export const legacyCallable = typeof legacy;
