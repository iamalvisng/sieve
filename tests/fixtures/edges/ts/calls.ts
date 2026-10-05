import * as ns from "./base";
import def from "./missing";
import "./side";
import { area as computeArea } from "./helpers";
import { Circle } from "./derived";

export function outer(): number {
  const b = new ns.Base();
  const c: Circle = new Circle();
  c.area();
  b.greet();
  computeArea();
  console.log(outer());
  return inner();

  function inner(): number {
    return 1;
  }
}
