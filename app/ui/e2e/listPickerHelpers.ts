import type { Page } from "@playwright/test";
import { expect } from "./fixture";

export const pickerButton = (page: Page, label: string) =>
  page.getByRole("button", { name: new RegExp(`^${label}: `) });

export async function choosePickerOption(page: Page, label: string, name: string) {
  await pickerButton(page, label).click();
  await page.getByRole("listbox", { name: label, exact: true }).getByRole("option", { name, exact: true }).click();
}

export async function confirmPickerWithArrows(
  page: Page, label: string, current: string, next: string, downSteps: number, expectNoWrite: () => Promise<void>,
) {
  const trigger = pickerButton(page, label);
  const list = page.getByRole("listbox", { name: label, exact: true });
  await expect(trigger).toHaveAccessibleName(`${label}: ${current}`);
  for (const action of ["Escape", "Tab", "Enter"]) {
    await trigger.press("Enter");
    await expect(list).toBeFocused();
    for (let step = 0; step < downSteps; step++) await page.keyboard.press("ArrowDown");
    await expectNoWrite();
    const nextOption = list.getByRole("option", { name: next, exact: true });
    await expect(list).toHaveAttribute("aria-activedescendant", await nextOption.getAttribute("id") ?? "");
    await expect(trigger).toHaveAccessibleName(`${label}: ${current}`);
    await page.keyboard.press(action);
    await expect(list).toBeHidden();
    if (action === "Enter") break;
    await expect(trigger).toHaveAccessibleName(`${label}: ${current}`);
    await expectNoWrite();
  }
  await expect(trigger).toHaveAccessibleName(`${label}: ${next}`);
}
