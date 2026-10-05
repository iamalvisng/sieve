export const handlers = {
  onClick() {
    return 1;
  },
};

export interface Shape {
  area(): number;
}

export type Id = string;

export enum Color {
  Red,
  Blue,
}

export abstract class Base {
  abstract go(): void;
}
