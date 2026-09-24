import { describe, expect, it } from "vitest";
import { enginesSummary, newSshHost, parsePort } from "./sshHosts";

describe("parsePort", () => {
  it("accepts 1-65535 integers", () => {
    expect(parsePort("22")).toBe(22);
    expect(parsePort(" 2222 ")).toBe(2222);
    expect(parsePort("65535")).toBe(65535);
  });
  it("rejects everything else", () => {
    expect(parsePort("0")).toBeNull();
    expect(parsePort("65536")).toBeNull();
    expect(parsePort("22.5")).toBeNull();
    expect(parsePort("abc")).toBeNull();
    expect(parsePort("")).toBeNull();
  });
});

describe("enginesSummary", () => {
  it("lists up to two, then counts", () => {
    expect(enginesSummary({})).toBe("");
    expect(enginesSummary({ muse: "/x" })).toBe("muse");
    expect(enginesSummary({ muse: "/x", codex: "/y", pi: "/z" })).toBe(
      "codex, muse (+1 more)",
    );
  });
});

describe("newSshHost", () => {
  it("trims and starts unprobed", () => {
    const host = newSshHost(" 10.0.0.2 ", " dev ", 22);
    expect(host.host).toBe("10.0.0.2");
    expect(host.user).toBe("dev");
    expect(host.lastOk).toBe(false);
    expect(host.engines).toEqual({});
    expect(host.id.length).toBeGreaterThan(0);
  });
});
