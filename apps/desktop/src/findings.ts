import type { Finding, FindingSeverity } from "./api";
import { t } from "./i18n";

/** Localized text for a finding code, with the backend's named values. */
export function findingTitle(finding: Finding): string {
  return t(`finding.${finding.code}.title`);
}

export function findingDetail(finding: Finding): string {
  const key = `finding.${finding.code}.detail`;
  const text = t(key, finding.params);
  return text === key ? "" : text;
}

export function findingHint(finding: Finding): string {
  return t(`finding.${finding.code}.hint`);
}

export function severityLabel(severity: FindingSeverity): string {
  return t(`severity.${severity}`);
}

export function findingsReport(findings: Finding[]): string {
  const lines = [t("report.title")];
  for (const finding of findings) {
    const subject = finding.subject ? ` — ${finding.subject}` : "";
    lines.push(`[${severityLabel(finding.severity)}] ${findingTitle(finding)}${subject}`);
    const detail = findingDetail(finding);
    if (detail) lines.push(`  ${detail}`);
    if (finding.evidence) lines.push(`  > ${finding.evidence}`);
    lines.push(`  ${t("report.hint")}: ${findingHint(finding)}`);
  }
  return lines.join("\n");
}
