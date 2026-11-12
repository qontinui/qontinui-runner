import { describe, expect, it } from "vitest";
import { skillExportNotice, skillImportNotice } from "./skillTransferNotice";

describe("skillImportNotice", () => {
  it("says nothing when every skill imported cleanly", () => {
    expect(skillImportNotice({ imported: 3, overwritten: 0, errors: [], refused: [] })).toBeNull();
    expect(skillImportNotice({ imported: 1 })).toBeNull();
  });

  it("names each refused playbook skill and the replacement", () => {
    const notice = skillImportNotice({
      imported: 1,
      errors: [],
      refused: [
        { code: "PLAYBOOK_TEMPLATE_RETIRED", skill: "sf-login" },
        { code: "PLAYBOOK_TEMPLATE_RETIRED", skill: "crm-notes" },
      ],
    });
    expect(notice).toContain('"sf-login", "crm-notes"');
    expect(notice).toContain("are playbook skills");
    expect(notice).toContain(".qontinui/contexts/");
  });

  it("says a single refused playbook skill in the singular", () => {
    const notice = skillImportNotice({
      refused: [{ code: "PLAYBOOK_TEMPLATE_RETIRED", skill: "sf-login" }],
    });
    expect(notice).toContain('"sf-login" is a playbook skill');
    expect(notice).not.toContain("are playbook skills");
  });

  it("names a refusal of another code by its code", () => {
    const notice = skillImportNotice({ refused: [{ code: "SOMETHING_NEW", skill: "x" }] });
    expect(notice).toContain('"x" (SOMETHING_NEW)');
  });

  it("surfaces import errors", () => {
    const notice = skillImportNotice({ errors: ["Failed to import skill 'a': boom"] });
    expect(notice).toContain("Failed to import skill 'a': boom");
  });

  it("surfaces import warnings, such as an unverified export checksum", () => {
    const warning =
      "Export checksum not verified: 1 skill(s) were refused before import, and the checksum covers them too";
    const notice = skillImportNotice({ imported: 1, warnings: [warning] });
    expect(notice).toContain(`Import warnings: ${warning}`);
  });
});

describe("skillExportNotice", () => {
  it("says nothing when nothing was withheld", () => {
    expect(skillExportNotice(undefined)).toBeNull();
    expect(skillExportNotice([])).toBeNull();
  });

  it("names a withheld skill, where its stored template went, and only then deleting it", () => {
    const notice = skillExportNotice([
      { skill_id: "user:sf-login", code: "STORED_SKILL_TEMPLATE_UNPARSEABLE", error: "e" },
    ]);
    expect(notice).toContain('"user:sf-login" because its stored template no longer parses');
    expect(notice).toContain('keeps it under "withheld"');
    expect(notice).toContain("delete that skill");
    const lower = notice!.toLowerCase();
    expect(lower.indexOf('"withheld"')).toBeLessThan(lower.indexOf("delete"));
  });

  it("says several withheld skills in the plural", () => {
    const notice = skillExportNotice([
      { skill_id: "user:sf-login", code: "STORED_SKILL_TEMPLATE_UNPARSEABLE", error: "e" },
      { skill_id: "user:crm-notes", code: "STORED_SKILL_TEMPLATE_UNPARSEABLE", error: "e" },
    ]);
    expect(notice).toContain('"user:sf-login", "user:crm-notes"');
    expect(notice).toContain("their stored templates no longer parse");
    expect(notice).toContain('keeps them under "withheld"');
    expect(notice).toContain("delete those skills");
  });
});
