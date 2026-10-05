export function buildIndex(): number[] {
  return sortItems([3, 1, 2]);
}

export function sortItems(items: number[]): number[] {
  return items.sort();
}

export function filterList(items: number[]): number[] {
  return items.filter((n) => n > 0);
}
