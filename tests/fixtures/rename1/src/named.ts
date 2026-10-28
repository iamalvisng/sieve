import { greet } from "./def";

// greet is called below
const label = "greet";
const other = "say greet now";

/**
 * @see greet
 * @param {typeof greet} f
 */
export function run(f: unknown): string {
  void f;
  return greet("a") + label + other;
}
