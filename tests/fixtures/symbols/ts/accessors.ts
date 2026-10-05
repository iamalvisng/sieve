export class Box {
  private _v = 0;

  get value() {
    return this._v;
  }

  set value(v: number) {
    this._v = v;
  }

  private _hidden() {
    return this._v;
  }
}
