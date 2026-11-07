// Fixture 5 (alias): `@fixture/aliased` maps to ./aliased through the jiti instance's `alias`.
import { value } from "@fixture/aliased";
import { deep } from "@fixture/aliased/deep";

export const aliased = { value, deep };
