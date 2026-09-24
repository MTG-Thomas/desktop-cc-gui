import { act, type ComponentType } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AppSettings } from "@/lib/ipc";

// Pages read their settings (and the pet list) on mount; the proxy keeps every
// other method inert for the module tree they pull in.
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
import { BetaFeaturesSection } from "./BetaFeaturesSection";
import { builtinSearchEntries } from "./builtin-search";
import { GeneralSection } from "./GeneralSection";
import { PerformanceDiagnosticsSection } from "./PerformanceDiagnostics";
import { ProxySection } from "./ProxySection";
import { UpdateSection } from "./UpdateSection";
import { ShortcutsSection } from "@/features/shortcuts/ShortcutsSection";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

/** Only the fields the row-based pages read; the rest of AppSettings is not
 *  touched by them. */
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

/** One entry per page that owns indexed rows: the component is rendered and
 *  its anchors are compared with that page's declared entries, so a row can
 *  neither be declared without rendering nor render without being indexed. */
const PAGES: { page: string; component: ComponentType }[] = [
  { page: "general", component: GeneralSection },
  { page: "proxy", component: ProxySection },
  { page: "shortcuts", component: ShortcutsSection },
  { page: "update", component: UpdateSection },
  { page: "betaFeatures", component: BetaFeaturesSection },
  { page: "diagnostics", component: PerformanceDiagnosticsSection },
];

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

/** Anchors the page actually painted; the awaited act lets the settings read
 *  land for pages that render their rows only afterwards. */
async function renderedAnchors(component: ComponentType): Promise<string[]> {
  const Page = component;
  await act(async () => {
    root.render(<Page />);
  });
  return [...container.querySelectorAll("[data-setting-anchor]")].map(
    (el) => el.getAttribute("data-setting-anchor") ?? "",
  );
}

describe("builtinSearchEntries", () => {
  it("covers every page this test knows how to render (and nothing else)", () => {
    const known = new Set(PAGES.map((page) => page.page));
    for (const entry of builtinSearchEntries) {
      expect([...known]).toContain(entry.page);
    }
  });

  for (const { page, component } of PAGES) {
    it(`${page}: declares exactly the rows it renders`, async () => {
      const rendered = await renderedAnchors(component);
      // A duplicated anchor would flash two rows at once and collide on one
      // React key.
      expect(new Set(rendered).size).toBe(rendered.length);
      const declared = builtinSearchEntries
        .filter((entry) => entry.page === page)
        .map((entry) => entry.anchor);
      expect([...rendered].sort()).toEqual([...declared].sort());
    });
  }

  it("resolves every label and breadcrumb key in both languages", () => {
    for (const entry of builtinSearchEntries) {
      // A typo resolves to the raw key at query time, so assert the copy
      // exists in the language pairs the app ships.
      for (const key of [entry.labelKey, entry.sectionKey]) {
        if (!key) continue;
        expect(i18n.exists(key, { lng: "zh" })).toBe(true);
        expect(i18n.exists(key, { lng: "en" })).toBe(true);
      }
    }
  });
});
