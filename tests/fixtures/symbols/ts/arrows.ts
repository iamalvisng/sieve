export const arrow = (n: number): number => n + 1;

const fnExpr = function (n: number) {
  return n;
};

export default function named() {
  function inner() {
    return 1;
  }
  return inner();
}
