import type { ReactElement } from "react";
import { Tooltip as RadixTooltip } from "radix-ui";
import styles from "./Tooltip.module.css";

/** How long the pointer rests on something before its tooltip shows */
const TOOLTIP_DELAY_MS = 400;

/** Shows `label` under `children` on hover or keyboard focus. */
export function Tooltip({ label, children }: { label: string; children: ReactElement }) {
  return (
    <RadixTooltip.Provider delayDuration={TOOLTIP_DELAY_MS}>
      <RadixTooltip.Root>
        <RadixTooltip.Trigger asChild>{children}</RadixTooltip.Trigger>
        <RadixTooltip.Portal>
          <RadixTooltip.Content className={styles.tooltip} side="bottom" align="start" sideOffset={6}>
            {label}
          </RadixTooltip.Content>
        </RadixTooltip.Portal>
      </RadixTooltip.Root>
    </RadixTooltip.Provider>
  );
}
