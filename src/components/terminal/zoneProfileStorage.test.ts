import { describe, expect, it } from "vitest";
import { profileResumeTabFields, ZONE_SESSION_PROVIDER } from "./zoneProfileStorage";

describe("profileResumeTabFields", () => {
  it("stamps the provider with the session, so a later retry can resolve its CLI profile", () => {
    expect(
      profileResumeTabFields({ zoneIndex: 2, claudeSessionId: "s-1", claudeConfigDir: "/h/.c" }),
    ).toEqual({
      claudeSessionId: "s-1",
      claudeConfigDir: "/h/.c",
      sessionProvider: ZONE_SESSION_PROVIDER,
    });
  });
});
