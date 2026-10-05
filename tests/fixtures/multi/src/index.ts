import { renderPage } from "../packages/alpha/src/render";
import { renderReport } from "../packages/beta/src/render";

export function buildAlphaPage(): string {
  return renderPage();
}

export function buildBetaReport(): string {
  return renderReport();
}
