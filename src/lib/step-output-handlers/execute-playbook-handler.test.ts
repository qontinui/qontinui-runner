/**
 * Unit tests for ExecutePlaybookHandler.
 *
 * The executor runs only the inline playbook body (`execute_playbook.rs`
 * reads `prompt_content`, wire field `content`). A stored step config may
 * still carry a `playbook_path` the executor never read, so nothing this
 * handler produces may name that path as the playbook that ran.
 */

import { describe, it, expect } from "vitest";
import { executePlaybookHandler } from "./execute-playbook-handler";

const BODY = "## Login\n### Step 1: open the page\n### Step 2: sign in";
const UNREAD_PATH = "/home/user/.qontinui/playbooks/never-read.md";

/** A stored step config carrying both: the body that ran and an unread path. */
const STALE_CONFIG = { playbook_path: UNREAD_PATH, content: BODY };

const RAW = {
  transitions: [
    { transition_id: "t1", transition_name: "open", success: true, duration_ms: 5 },
    { transition_id: "t2", transition_name: "sign in", success: true, duration_ms: 7 },
  ],
  final_states: ["dashboard"],
  completed: true,
};

describe("ExecutePlaybookHandler", () => {
  describe("parseOutput", () => {
    it("carries the inline body and no playbook_path", () => {
      const out = executePlaybookHandler.parseOutput(RAW, STALE_CONFIG, { success: true });

      expect(out.content).toBe(BODY);
      expect(out).not.toHaveProperty("playbook_path");
      expect(JSON.stringify(out)).not.toContain(UNREAD_PATH);
    });

    it("names the step from the body, never from the unread path", () => {
      const out = executePlaybookHandler.parseOutput(RAW, STALE_CONFIG);

      expect(out.step_name).toBe("## Login");
      expect(out.step_name).not.toContain("never-read");
    });

    it("falls back to the step type when there is no body", () => {
      const out = executePlaybookHandler.parseOutput(RAW, { playbook_path: UNREAD_PATH });

      expect(out.step_name).toBe("execute_playbook");
    });
  });

  describe("summarizeForAI", () => {
    it("names the inline body that ran, never the unread path", () => {
      const out = executePlaybookHandler.parseOutput(RAW, STALE_CONFIG, { success: true });
      const summary = executePlaybookHandler.summarizeForAI(out);

      expect(summary).toContain("- **Playbook:** inline (3 lines)");
      expect(summary).toContain("**Playbook Content:**");
      expect(summary).toContain(BODY);
      expect(summary).not.toContain(UNREAD_PATH);
    });

    it("omits the source line and the body block when there is no body", () => {
      const out = executePlaybookHandler.parseOutput(RAW, { playbook_path: UNREAD_PATH });
      const summary = executePlaybookHandler.summarizeForAI(out);

      expect(summary).not.toContain("- **Playbook:**");
      expect(summary).not.toContain("**Playbook Content:**");
      expect(summary).not.toContain(UNREAD_PATH);
    });
  });

  describe("toTestConfig", () => {
    it("replays the inline body and records no path", () => {
      const out = executePlaybookHandler.parseOutput(RAW, STALE_CONFIG, { success: true });
      const test = executePlaybookHandler.toTestConfig(out);

      expect(test).not.toBeNull();
      expect(test?.config_value).toMatchObject({
        content: BODY,
        expected_transitions_total: 2,
        expected_completed: true,
      });
      expect(test?.config_value).not.toHaveProperty("playbook_path");
      expect(test?.description).toBe("Playbook config for inline playbook (2 transitions)");
    });

    it("has nothing to replay without a body, whatever path the config carries", () => {
      const out = executePlaybookHandler.parseOutput(RAW, { playbook_path: UNREAD_PATH });

      expect(executePlaybookHandler.toTestConfig(out)).toBeNull();
    });
  });
});
