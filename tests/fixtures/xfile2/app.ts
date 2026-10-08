import { area as a } from "./named";
import * as shapes from "./named";
import build from "./def_fn";
import mk from "./def_ident.js";
import cr from "./def_clause";
import { default as b2 } from "./def_fn";
import m from "./barrel";
import * as fs from "node:fs";
import { gone as g } from "./decoy";

type Props = { shapes: any };

export function run() {
  a();
  shapes.perimeter();
  build();
  mk();
  cr();
  b2();
  m();
}

function local(shapes: any) {
  shapes.perimeter();
}

function destructured({ shapes }: Props) {
  shapes.perimeter();
}

const xs = [() => 1];
for (const a of xs) {
  a();
}

const f = function a() {
  return a();
};

export function viaLocal() {
  const { a } = { a: () => 1 };
  return a();
}

export function negatives() {
  try {
    xs.length;
  } catch (a) {
    a();
  }
  shapes.a.fn();
  shapes["perimeter"]();
  fs.readFileSync("x");
  g();
}
