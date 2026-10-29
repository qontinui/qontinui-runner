import type { PaneLabel } from "./paneLabel";
import { usePaneLabel, type DisplayTitleTab } from "./displayTitle";

/**
 * Renders a pane label (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 4): the primary line `<name> · <id8>`, followed by a smaller,
 * muted secondary line with the activity title.
 *
 * Truncation must never hide the identifying id. The NAME span truncates
 * (`min-w-0 truncate`). The ` · <id8>` suffix is `shrink-0 whitespace-nowrap`,
 * so it stays visible however narrow the header gets. The activity line
 * shrinks before the primary line does (its `flex-shrink` is larger).
 *
 * UI Bridge annotations, keyed by tab id so every pane's label is addressable:
 * `pane-label-<tabId>` (the whole label), `pane-label-primary-<tabId>`,
 * `pane-label-id-<tabId>` (the id suffix), and `pane-label-activity-<tabId>`.
 * The full text of both lines is in `title`, so the name is reachable even
 * when it truncates.
 */
export function PaneLabelText({
  label,
  tabId,
  primaryClassName = "",
  secondaryClassName = "text-[9px] text-[#565f89]",
  showSecondary = true,
}: {
  label: PaneLabel;
  tabId: string;
  primaryClassName?: string;
  secondaryClassName?: string;
  showSecondary?: boolean;
}) {
  const secondary = showSecondary ? label.secondary : undefined;
  const full = secondary ? `${label.primary} — ${secondary}` : label.primary;
  return (
    <span
      data-ui-id={`pane-label-${tabId}`}
      className="flex items-baseline gap-1.5 min-w-0 overflow-hidden"
      title={full}
    >
      <span
        data-ui-id={`pane-label-primary-${tabId}`}
        className={`flex min-w-0 ${primaryClassName}`}
        style={{ flexShrink: 1 }}
      >
        {label.name !== undefined && label.idSuffix !== undefined ? (
          <>
            <span className="truncate min-w-0">{label.name}</span>
            <span
              data-ui-id={`pane-label-id-${tabId}`}
              className="shrink-0 whitespace-nowrap font-mono"
            >
              {label.idSuffix}
            </span>
          </>
        ) : (
          <span className="truncate min-w-0">{label.primary}</span>
        )}
      </span>
      {secondary && (
        <span
          data-ui-id={`pane-label-activity-${tabId}`}
          className={`truncate min-w-0 ${secondaryClassName}`}
          style={{ flexShrink: 4 }}
        >
          {secondary}
        </span>
      )}
    </span>
  );
}

/** {@link PaneLabelText} for one tab, resolved through the shared registry store. */
export function TabPaneLabel({
  tab,
  primaryClassName,
  secondaryClassName,
  showSecondary,
}: {
  tab: DisplayTitleTab & { id: string };
  primaryClassName?: string;
  secondaryClassName?: string;
  showSecondary?: boolean;
}) {
  const label = usePaneLabel(tab);
  return (
    <PaneLabelText
      label={label}
      tabId={tab.id}
      primaryClassName={primaryClassName}
      secondaryClassName={secondaryClassName}
      showSecondary={showSecondary}
    />
  );
}
