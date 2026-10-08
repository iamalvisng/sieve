import { helper as h1, renamed } from "./named_bar";
import { helper } from "./star_bar";
import * as chain from "./star_chain";
import d1, { libDef } from "./def_bar";
import d2 from "./def_bar2";
import { tools } from "./ns_bar";
import { cb } from "./cyc_a";
import { helper as hc } from "./conflict";
import { other as o3, helper as h3 } from "./local_bar";
import { tools2 } from "./prop_bar";
import { helper as hm } from "./mix_bar";

export function run() {
  h1();
  renamed();
  helper();
  chain.other();
  d1();
  libDef();
  d2();
  tools.helper();
  cb();
  o3();
  tools2.fn();
}

export function run_hc() {
  hc();
}

export function run_h3() {
  h3();
}

export function run_hm() {
  hm();
}
