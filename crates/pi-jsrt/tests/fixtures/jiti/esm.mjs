// Fixture 1 (ESM): a native ES module with a default and named exports and module state.
export let loads = 0;
loads += 1;
export const kind = "esm";
export default function greet(name) {
  return `hello ${name}`;
}
