export function renderMenu(): string {
  return layoutGrid();
}

export function layoutGrid(): string {
  return paintCanvas();
}

// paintCanvas does not format a row; it fills pixels on the grid.
export function paintCanvas(): string {
  return "canvas";
}
