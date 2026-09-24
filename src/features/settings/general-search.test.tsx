import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AppSettings } from "@/lib/ipc";

// 通用 reads its settings and the pet list on mount; the proxy keeps every
// other method inert for the module tree the section pulls in.
const { getAppSettings, listPets } = vi.hoisted(() => ({
  getAppSettings: vi.fn(),
  listPets: vi.fn(async () => []),
}));
vi.mock("@/lib/ipc", () => ({
  ipc: new Proxy(
    { getAppSettings, listPets },
    {
      get: (target, prop) =>
        prop in target ? Reflect.get(target, prop) : async () => null,
    },
  ),
}));

import i18n from "@/lib/i18n";
import { GeneralSection } from "./GeneralSection";
import { generalSearchEntries } from "./general-search";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

/** Only the fields 通用 renders rows for; the rest of AppSettings is not read
 *  on this page. */
const SETTINGS = {
  theme: "light",
  titlebar: "native",
  language: "zh",
  sidebarThreadLimit: 5,
  composerSendShortcut: "enter",
  thinkingAutoCollapse: true,
  petEnabled: false,
  petScale: 1,
  petId: "",
} as unknown as AppSettings;

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  getAppSettings.mockResolvedValue(SETTINGS);
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

/** Anchors of the rows the page actually rendered. The page paints them after
 *  its settings read lands, so the render has to be awaited. */
async function renderedAnchors(): Promise<string[]> {
  await act(async () => {
    root.render(<GeneralSection />);
  });
  return [...container.querySelectorAll("[data-setting-anchor]")].map(
    (el) => el.getAttribute("data-setting-anchor") ?? "",
  );
}

describe("generalSearchEntries", () => {
  it("indices exactly the rows 通用 renders — both directions of drift fail", async () => {
    const rendered = await renderedAnchors();
    // A duplicated anchor would make two rows flash at once and two results
    // collide on one key.
    expect(new Set(rendered).size).toBe(rendered.length);
    expect([...rendered].sort()).toEqual(
      generalSearchEntries.map((entry) => entry.anchor).sort(),
    );
  });

  it("points every entry at 通用 and resolves its keys in both languages", () => {
    for (const entry of generalSearchEntries) {
      expect(entry.page).toBe("general");
      for (const key of [entry.labelKey, entry.sectionKey]) {
        // A typo resolves to the raw key at query time, so assert the copy
        // exists in the language pairs the app ships.
        expect(i18n.exists(key, { lng: "zh" })).toBe(true);
        expect(i18n.exists(key, { lng: "en" })).toBe(true);
      }
    }
  });
});
