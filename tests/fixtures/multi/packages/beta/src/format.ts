export function renderTable(): string {
  return formatRow();
}

export function formatRow(): string {
  return formatCell();
}

export function formatCell(): string {
  return "cell";
}
