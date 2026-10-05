import { Base, Shape } from "./base";

export class Circle extends Base implements Shape {
  area(): number {
    return this.greet().length;
  }

  describe(): string {
    return super.greet();
  }
}
