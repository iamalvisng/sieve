import { greet } from "./def";

/** @type {typeof greet} */
export const g = greet;

/** Uses {@link greet}. */
export function viaJs() {
  return g("j");
}
