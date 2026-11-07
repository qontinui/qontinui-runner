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

  it("names a refusal of another code by its code", () => {
    const notice = skillImportNotice({ refused: [{ code: "SOMETHING_NEW", skill: "x" }] });
    expect(notice).toContain('"x" (SOMETHING_NEW)');
  });

  it("surfaces import errors", () => {
    const notice = skillImportNotice({ errors: ["Failed to import skill 'a': boom"] });
    expect(notice).toContain("Failed to import skill 'a': boom");
  });
});

describe("skillExportNotice", () => {
  it("says nothing when nothing was withheld", () => {
    expect(skillExportNotice(undefined)).toBeNull();
    expect(skillExportNotice([])).toBeNull();
  });

  it("names each withheld skill", () => {
    const notice = skillExportNotice([
      { skill_id: "user:sf-login", code: "STORED_SKILL_TEMPLATE_UNPARSEABLE", error: "e" },
    ]);
    expect(notice).toContain('"user:sf-login"');
    expect(notice).toContain("no longer parses");
  });
});
