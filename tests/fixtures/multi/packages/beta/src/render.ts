export function renderReport(): string {
  return renderChart() + exportPdf();
}

export function renderChart(): string {
  return "chart";
}

export function exportPdf(): string {
  return "pdf";
}
