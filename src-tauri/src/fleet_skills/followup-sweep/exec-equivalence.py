#!/usr/bin/env python3
"""exec-equivalence.py -- is each file a diff touches NON-EXECUTABLE?

A sidecar of the follow-up sweep skill (plan
2026-10-10-small-followups-are-recorded-but-nothing-burns-them-down, Phase 2
and design decision D7). The sweep batches a follow-up only when its whole fix
is proven non-executable, and this script is that proof: "trivial" is decided
per file by a checker, never asserted by a model. It travels with the skill
into the runner's skill bundle, so it reaches its own helpers ONLY through the
`lib/` directory beside it, never through any checkout path.

USAGE
  python3 exec-equivalence.py --repo-root <dir> --base <ref> [--head <ref>]
                              [--code-paths <json>] [--json]

  --repo-root <dir>   any directory inside the git work tree to examine.
  --base <ref>        the tree the diff is measured FROM. Taken literally, as
                      `git diff <base> <head>` takes it: pass a merge-base when
                      <head> is a branch that may have fallen behind <base>.
  --head <ref>        the tree the diff is measured TO. Omitted: the WORKING
                      TREE, judged as each of two commits would record it,
                      the worse reading standing for each path. One is
                      `git add -A`'s: tracked changes, staged or not, and
                      untracked files that are not ignored, each read whole
                      as git would store it -- end-of-line conversion and
                      working-tree-encoding applied, so a CRLF checkout
                      compares as a commit would store it. A file
                      `git rm --cached` left on disk and not ignored is read
                      that way too. The other is a commit of the index: what
                      is staged, wherever the working tree no longer holds
                      it. In either mode git attributes that change what a
                      checkout holds are fail-closed (round 28): a changed
                      path whose `filter`, `text`, `eol`, `crlf`,
                      `working-tree-encoding` or `ident` is set on any side,
                      or differs between sides, is unknown; a change to any
                      `.gitattributes`, at any depth, makes every path
                      unknown; and so does a non-empty attributes source the
                      diff cannot show ($GIT_DIR/info/attributes,
                      core.attributesFile, $(prefix)/etc/gitattributes unless
                      GIT_ATTR_NOSYSTEM; a git before 2.42 cannot name the
                      last, so it reads unknown). Replace refs, GIT_ATTR_SOURCE and
                      attr.tree are never honoured.
  --code-paths <json> a JSON array of glob strings: the repo's declaration of
                      which paths are code (globset defaults, as coord matches
                      them). Omitted: read from coord's
                      GET /coord/post-merge-followup-scope?repo=<owner/name>
                      under a credential for the tenant PROVEN to own the
                      repo's origin.
  --json              one JSON document on stdout instead of the line report.

VERDICTS -- one per changed path
  non_executable  proven: the change cannot alter what the repo does.
  executable      NOT proven inert: the change does, or may, alter behaviour
                  (coord's proof refused it, a token changed, a doctest or a
                  tool directive changed, a code file was added or removed).
  unknown         no proof could be attempted: no coord answer, no grammar,
                  no code_paths oracle. Never rendered as either of the above.

EXIT CODES
  0  every changed file is non_executable (a diff with no changed file also
     exits 0, and the report says `files=0`)
  4  at least one file is executable
  5  at least one file is unknown and none is executable
  2  usage, git or internal error, or --help -- no verdict was produced

THE ARMS (design decision D7)
  Python (.py .pyi .pyw, or a python shebang): base and head are POSTed to
    coord's POST /coord/exec-equivalence/python {path, base, head, profile} --
    coord's own non-executable-edit proof, not a copy of it. `profile` is
    `migration` for a path coord's merge engine treats as a migration
    (`alembic/versions/`, `migrations/`) and `general` otherwise. On top of a
    `true` this script refuses, client side: any changed statement string
    containing `>>>` (a repo may collect doctests), a changed shebang line, and
    a changed set of tool-directive comments (`# type:`, `# noqa`,
    `# pragma: no cover`, ...). No answer is `unknown`, never non_executable.
  Rust, TypeScript/TSX, JavaScript, shell: tree-sitter grammars from
    tree-sitter-language-pack, PINNED (TREE_SITTER_PACK_PIN below). Both sides
    are reduced to their concrete syntax tree with comment nodes removed --
    string-like nodes are compared as one atomic token, so whitespace inside a
    literal is significant -- and the two reductions must be equal. On top:
      * a TOOL DIRECTIVE in a comment (a shebang, `@ts-expect-error`,
        `eslint-disable`, `/*#__PURE__*/`, `# shellcheck`, JSDoc type tags in
        a JavaScript file, ...) stays IN the comparison at its position;
      * TS/JS: whether each gap between two tokens holds a line terminator is
        compared too (automatic semicolon insertion makes it significant);
      * Rust: a doc comment's PRESENCE is compared (a doc comment is an
        attribute: adding or removing one can stop a build). Its text may
        change only under a POSITIVE WHITELIST -- anything it does not name
        is executable, so a construct nobody thought of fails closed. An
        item's doc group is every fragment of the same kind up to its item,
        across attributes and directive comments (rustdoc joins them; over-
        merging is safe, under-merging is not). A CHANGED group is
        non_executable only when, on BOTH sides:
          (i)   it is solely consecutive `///` lines, or solely consecutive
                `//!` lines, with nothing between its first and last fragment
                -- no attribute, no comment of any kind, no `#[doc]`. ANY
                `/** */` or `/*! */` fragment refuses it outright (rustdoc's
                star strip and its unindent across fragment kinds are not
                modelled);
          (ii)  no raw line, measured after removing ONLY the marker (never
                after the group's own unindent, which can only remove
                indentation), has a leading tab or 4+ leading spaces, or
                starts (after at most 3 spaces) with a fence (``` or ~~~),
                `<`, `[^`, `>`, a list marker or `|` -- so no fence, indented
                code, HTML block, footnote, quote, list or table exists to
                hold or move a doctest;
          (iii) no line holds a CR, another C0 control (TAB aside), DEL, a
                C1 control, or U+2028 / U+2029;
          (iv)  it is not inside a macro (a `macro_rules!` body or any macro
                invocation's tokens), and an outer group documents an item
                kind in RUST_DOC_ITEMS -- never a `mod` item, whose inner
                docs join its outer docs, nor a macro invocation;
          (v)   an inner group sits directly in the file, and a non-root
                file's parent `mod <name>;` is found and carries no doc
                fragment of its own;
          (vi)  every attribute on the item and on each enclosing item is in
                RUST_INERT_ATTRS (a `derive` only of RUST_INERT_DERIVES): any
                other may be a proc macro that reads docs (clap's --help text,
                displaydoc's Display, schemars' schema);
          (vii) an outer group's item holds, ANYWHERE in its subtree, no inner
                doc comment and no attribute naming `doc`: rustdoc joins an
                item's `///` with the `//!` / `#![doc]` at the top of its body
                and unindents them together, so a `///` edit can move a body
                line into a doctest (`rust_doc_joined_body_inner`).
        A changed fence or indented block is named rust_doc_code_block before
        the whitelist runs;
      * shell: a gap is blank only when it holds bash's own blanks -- space,
        tab and newline (tree-sitter puts a line continuation in a node, not
        a gap). tree-sitter-bash
        also skips CR, VT and FF, which bash keeps inside a word (`a`, VT,
        `b` is one argument, `a b` two). And a tree-sitter comment is compared
        as code unless bash reads it as one: a `#` starts a comment only at
        the start of a word, never after CR, VT, FF, an escaped byte or a
        continuation, and never inside `$(( ))`, `(( ))`, `$[ ]`, a C-style
        `for (( ))` header (a comment after its `))` is one), `${ }` (offsets
        and subscripts included), an assignment's `[ ]` subscript or a
        compound assignment's `[ ]` key (`([k #x<LF>]=v)`, where tree-sitter
        splits the key) -- unless a command substitution,
        subshell or body nested there starts a fresh command line;
      * shell: a script that reads its own source (`sed ... "$0"` for --help)
        makes every comment executable, and so does one that names its own
        path or text anywhere while handing a text reader, or a `<` redirect,
        an expanded argument (`SELF="$0"; sed -n 2p "$SELF"`). It names itself
        through `$0`, `$_`, BASH_SOURCE, BASH_ARGV0, BASH_ARGV, `${@:..}` /
        `${*:..}`, an indirect `${!x}`, `caller`, a `/proc/` or `/dev/fd/`
        path (bash holds the script open on fd 255), or a literal holding its
        own file name. A command whose name is computed, and a wrapper handed
        a computed word, count as readers; a word bash expands as a pattern
        (`/bin/ca?`, `c[a]t`, `{a,b}`) is computed, and a `$'..'` word is
        read decoded and cut at its first NUL, as bash reads it. Text handed to `eval`, `trap`,
        `alias` or a shell's `-c` is parsed and checked with the script --
        the words joined, and each word alone, so an option word (`--`,
        `-o errexit`) hides nothing; text there that is not a literal, or does
        not parse, is executable. So is a script that may turn on bash's
        verbose mode, which echoes every line read, comments included: a `#!`
        flag with `v` or `verbose`, `set -v` / `set -o verbose` / `shopt -o
        verbose` (wrapped or not, or with an option word that is not a
        literal), checked in evaluated text too;
      * TS/JS: Function.prototype.toString returns a function's or class's
        exact text, comments and whitespace included, and any string
        conversion calls it, so a change anywhere inside a function, method,
        arrow function or class is executable;
      * TS/JS and Rust: a file that names its own path makes every comment
        executable -- `__filename`, `__dirname`, `import.meta`, `process.argv`,
        `process.mainModule`, `require.main` / `.cache`, `module.filename` /
        `.id` / `.path` / `.constructor`, `currentScript`, an error's
        `.stack`, a CommonJS wrapper reached by `.callee`, `.caller` or a
        module-level `arguments` (its toString() is the whole module) --
        each property spelled `o.p`, `o["p"]` or destructured (`{p} = o`),
        every name read with its `\\u` escapes decoded (`__file\\u006eame`),
        and any computed property of `module`, `process`, `require` or
        `document` (a JSX attribute string or text read with its character
        references decoded by the XHTML table JSX uses, JSX text also as
        Babel, tsc and esbuild each trim and join its lines); a string whose last path
        component an extensionless
        specifier resolves here by (`./j`, and `./dir` or `./` for an `index`
        file); Rust's `file!()` (also inside another macro's tokens),
        `Location`, CARGO_MANIFEST_DIR; or a literal holding the file's name
        (for TS/JS, its name less its last extension under any extension it
        is emitted as: `a.test.ts` is `a.test.js`);
      * shell: a here-document tree-sitter ends on a line bash would not
        (`EOF ` with a trailing space, a space-indented `<<-` end, a CR), or
        that bash would end earlier, is executable: a line parsed as a
        comment may be heredoc data;
      * a parse error on exactly one side is executable, on both is unknown.
    A missing pack, a pack at another version, or a missing grammar is
    `unknown`, never non_executable.
  Any other file: by the repo's declared code_paths. Outside them:
    non_executable, unless it carries a TEXT DOCTEST or Rust source may
    compile it in. code_paths says which paths are code, not what a doctest
    collector (pytest --doctest-glob, Sphinx's doctest builder) reads, and
    D7's arming forbids calling a doctest-bearing edit non_executable, so a
    marker decides it wherever the file lies: a changed paragraph --
    delimited only by lines of spaces and tabs, doctest's own rule --
    holding a `>>>` is executable; an added or deleted text holding a `>>>`
    or a Sphinx doctest directive (`.. testsetup::`, `.. testcode::`,
    `.. doctest::`, `.. testcleanup::`, `.. testoutput::`, or MyST's
    `{testsetup}` ...) is executable (`doctest_marker_in_text:<added|
    deleted>:<marker>`); and any other edit to a text carrying a Sphinx
    directive is unknown (`doctest_marker_in_text:sphinx_doctest_directive`),
    because a `testsetup` or `testcode` block holds no `>>>` and feeds
    examples anywhere in the file, so no paragraph rule localises it.
    Directive names match case-insensitively (docutils lowercases them); a
    NUL-bearing text is still read (doctest reads it as text); a text that
    may be UTF-16/32 (a BOM, or NULs) is also decoded, and a marker found
    only that way, or a wide BOM alone, is `wide_encoding` -- executable when
    added or deleted, unknown when edited. A symlink or a submodule pointer
    outside code_paths is unknown (`symlink_outside_code_paths`,
    `submodule_outside_code_paths`): a collector follows the link or walks
    the submodule, so what it names is not proven inert by where it lies.
    Every .rs file naming an include-family
    macro (include, include_str, include_bytes) or another compile-time
    reader is tokenised (with Rust's own Pattern_White_Space, so U+200E /
    U+200F between tokens hide nothing), and every occurrence of an include
    name must be a PLAIN invocation: `name!("literal")`, optionally
    `std::`/`core::`-qualified, outside any `macro_rules!` body, its literal
    a relative path with no escape that resolves -- against the including
    file's directory, by exact spelling -- to a tracked file. A plain include
    naming the changed file makes it executable; a `#[doc = include_str!]`
    one keeps its code blocks executable, refuses block context, and is
    executable outright when its item carries any OTHER doc fragment (rustdoc
    joins them). A file a plain `include!` names is scanned in turn. A
    SOURCE file judged non_executable by its own arm is re-judged the same
    way (an embedded `tool.sh` carries its comments into the program). An
    include of `concat!(env!("OUT_DIR"), "..")` names the build directory,
    never a repository file, and is skipped (a build script copying a file
    there is the R2 residual). ANY
    other occurrence -- a `use` path or rename, a `concat!` / `env!` or other
    non-literal argument, a macro body, a name handed to a macro, an
    unresolvable path, a `#[path]` module read from a non-.rs file -- makes
    EVERY candidate unknown (`included_by_rust_opaque:<rs>:<why>`), since it
    may name any of them: not only non-code files but every SOURCE file its
    own arm judged non_executable too. The usability cost is real: a repo
    that includes through the common `concat!(env!("CARGO_MANIFEST_DIR"),
    ..)` pattern anywhere gets unknown for EVERY comment edit in it.
    Beyond the code tokens, the RAW TEXT of every scanned .rs file --
    comments, doc comments, the inside of string literals -- and the whole
    text of EVERY file a plain `include_str!` / `include_bytes!` names,
    wherever the include sits (a README behind `#![doc = include_str!(..)]`,
    or one a macro may turn into doc text), is searched for the same names:
    rustdoc compiles a doctest out of any of them, so a name found there is
    opaque too (`raw_text:<name>` / `attribute_included_text:<path>:<name>` /
    `included_text:<path>:<name>`), as is a name only a string escape spells
    (`escaped_name`). A `doc = ..` whose value is not a single plain string
    literal or a plain `include_str!("literal")` is opaque wherever it is
    written -- in code, judged on the comment-free token stream
    (`token:doc_attribute_non_literal`), and in comment or string text, with
    comments allowed around the `=` (`doc_attribute_non_literal`): a
    `concat!` may assemble an include out of pieces. So is any attribute a
    `macro_rules!` body builds from a metavariable (`#[$m]`, `#[doc = $x]`,
    `#$m`, or a bracket group right after a metavariable, `$h [$d = $s]`:
    `macro_attribute_metavariable`), since it may carry any meta; so are
    two metavariables in a macro body side by side or across one `!`
    (`$h $t`, `$p ! $q` -- a `#` and a group, an outer or an inner
    attribute: `macro_adjacent_metavariables`) and a `#` token anywhere in
    code that opens no attribute (`mk!(#, doc, ..)`: `stray_pound_token`),
    since a macro may place it before any group; so is a `path = ..` meta
    in comment or string text (a doctest's `#[path = "x"] mod m;`); and
    a plain include handed to any macro but a std expression macro
    (`assert_eq!`, `format!`, ...) that nothing can have rebound
    (`include_in_macro_arguments`), since that macro may make it doc text.
    A std name is rebound by a `macro_rules!` or `macro` (decl_macro) of
    that name or a `use` naming it (`use x::documented as assert_eq;`) in
    ANY tracked file of the tree (any name: `include!` compiles a file of
    any extension), and every std name is revoked by a glob `use` in the
    scanning file or by the word `macro_use` anywhere in the tree (however
    the attribute is spelled: `cfg_attr`, comments, paths), which can put a
    same-named macro in textual scope with no visible name. A raw
    identifier `r#name` is read as `name` by every one of these rules. An
    include path is resolved the way the FILESYSTEM resolves it -- `..` is
    applied after links are followed, never by collapsing the string -- so
    a path that reaches a tracked symlink at any component is opaque
    (`include_through_symlink`), and one that applies `..` to a directory
    this side does not track is opaque too (`unresolved_path`). ANY
    non_executable file a tracked symlink names, directly or through a
    linked directory, on either side (each side's link resolved against
    that side's tree, so a retargeted link names both targets), is unknown
    (`aliased_by_symlink:<link>`): every reader of the link -- an include,
    a `mod` through a linked `.rs`, a doctest collector -- reads it. A link
    target is followed the same way, and NO target is ever read as leaving
    the repository: one that climbs above the root by `..` may land back in
    the checkout under the name it is cloned as (`../../widget/x` in a
    clone named `widget`), which no commit records, so it -- like any
    target not followed exactly, any absolute one not landing on this root
    here, any one that reaches nothing this side tracks by that exact
    spelling (`../GEN/gen.txt` and `../gen/gen.txt.` are gen/gen.txt on
    NTFS / APFS), and any one spelled with a backslash or a drive letter
    (one predicate, `path_spelling_kind`, decides that for links and
    include literals alike) -- aliases every candidate
    (`aliased_by_symlink:<link>`).
    DOCTESTS ARE CODE: every Rust doctest a scanned file may carry -- each
    item's joined doc comments and `#[doc]` literals, every string literal
    in an attribute, a macro body or a non-std macro's arguments, and the
    whole text of every file a plain `include_str!` names -- is read the way
    rustdoc reads it (fenced blocks it treats as Rust, indented blocks,
    hidden `# ` lines shown; where block context is ambiguous, also with
    `>` markers stripped and every line dedented) and given the SAME token
    scan as a repository file, so a macro in a doctest that builds
    `#[$a = ..]` or `$i!(..)` out of a name handed in as a plain word is
    caught as it is in code (`<where>:doctest:<why>`); any include inside
    one is opaque (it resolves against no file here), and doctests nested
    more than DOCTEST_MAX_DEPTH deep are opaque outright. A fenced block is
    classed by a conservative superset of rustdoc's parse, in its ORDER: it
    is Rust when `rust` appears anywhere, when a Rust attribute (`no_run`,
    `should_panic`, `ignore`, `edition..`, an E-code, ...) comes before the
    first word rustdoc does not know (`no_run,sh` is a doctest, `sh,no_run`
    is not), when every word is an attribute, or -- fail-closed -- when the
    info string holds any character outside `[A-Za-z0-9_.=:-]`, comma and
    whitespace (`{..}` groups, `(..)` comments, quotes, backslash escapes,
    entity references); only otherwise is it skipped.
    The .rs files are found with `git grep -a --no-textconv`, so a blob git
    deems binary (a NUL byte, a `-diff` attribute) is searched like any
    other, and read with `git cat-file`, never through a textconv filter.
    So does any reader in OTHER_READER_IDENTS / OTHER_READER_MACROS /
    OTHER_READER_ATTR_ARGS (include_dir!, sqlx's query_file! and migrate!,
    rust-embed, askama's #[template(path)], pest's #[grammar], tauri's
    generate_context!, ...): `compile_time_reader:<name>:<rs>`. That list is
    EXTENSIBLE and is a BOUNDARY: a reader it does not name -- any proc
    macro, any build script -- is not seen (see RESIDUALS). Inside
    them: Markdown, JSON, CSV/TSV and binary files are executable (no inert
    sub-language: a command prompt is code); anything else is unknown (no
    grammar). Undeclared, unreadable, or an oracle with a glob that did not
    compile and no match: unknown.
  A file whose language arm applies is judged by that arm whatever code_paths
  says: a mis-declared oracle can never turn a source edit non_executable.
  Added, deleted, renamed (seen as delete + add), symlinked, submodule and
  mode-changed paths are executable when they are code, non_executable when
  they lie outside code_paths, and unknown when the oracle is.

CREDENTIAL
  A coord bearer is staged by lib/stage-coord-bearer.sh, which sources
  lib/coord-tenant-credential.sh (a byte-identical copy of the fleet's
  tenant-proof library): $COORD_DEVICE_JWT, ~/.qontinui/coord-device-jwt, the
  local runner's mint, then coord's anonymous mint -- each source selected only
  when it is fresh and claims the tenant asked for. code_paths are read under
  the tenant PROVEN to own the origin repo; the Python route is auth-only, so
  without code_paths to read any staged tenant credential serves it. The bearer
  never reaches argv, and is sent only to an https or loopback coord. bash is
  located by lib/bash_resolve.py (never Windows' WSL launcher), and every git
  call runs after lib/git_scope.py has stripped an inherited GIT_DIR.

ENVIRONMENT
  EXEC_EQ_COORD_URL  FIXTURE MODE: the coord base url. Wins over COORD_HTTP_URL,
                     so a hermetic test points every coord call -- the proof,
                     the code_paths read and the credential's ownership probe --
                     at a canned-response fixture.
  COORD_HTTP_URL     the coord base url (default https://coord.qontinui.io).
  QONTINUI_TENANT_ID, QONTINUI_MACHINE_ID, COORD_DEVICE_JWT, CTC_NO_MINT,
  CTC_NO_RUNNER      read by lib/coord-tenant-credential.sh (see its header).
  QONTINUI_BASH      read by lib/bash_resolve.py.

RESIDUALS, stated so a non_executable is not over-read
  * Not a residual, a cost (round 28): git attributes are fail-closed as a
    class. A changed path is unknown when any of `filter`, `text`, `eol`,
    `crlf`, `working-tree-encoding` or `ident` is set for it on any side
    (`checkout_attribute:<attr>=<value>`), or reads differently between
    sides (`checkout_attribute_differs:<attr>`); a repository with
    `* text=auto` therefore reads unknown throughout. Every path is unknown
    when any `.gitattributes`, at any depth or fold-twin spelling, is among
    the changes (`attributes_source_changed:<path>`), or when
    $GIT_DIR/info/attributes or the core.attributesFile (default
    $XDG_CONFIG_HOME/git/attributes) or the system file
    $(prefix)/etc/gitattributes holds a line (`local_attributes_source:<file>`),
    since none is in a diff. The system file is read unless
    GIT_ATTR_NOSYSTEM is true, at the path `git var GIT_ATTR_SYSTEM` names;
    a git older than 2.42 cannot name it, so every path is unknown there
    (`local_attributes_source:GIT_ATTR_SYSTEM:unresolved`).
    Conversions that come from config alone (core.autocrlf, core.eol) are
    the clone's, not the commit's, and are not modelled; the working tree is
    stored through them as `git add` would.
  * Not a residual, but a cost worth stating: ONE tracked symlink whose
    target the checker cannot follow exactly makes EVERY non-code candidate
    unknown. That covers a target that climbs above the root, an absolute
    target not on this root (round 13), a dangling target that reaches
    nothing tracked by its exact spelling, a target spelled with a backslash
    or a drive letter (round 14), a target through another link or an
    untracked directory, and a target whose path has a case-fold or NFC/NFD
    twin among the tracked paths (round 15). The checker cannot prove what
    such a target names, so it never claims the target names nothing here.
  * Also a cost: a changed file with a tracked fold twin (`config/app.yaml`
    and `Config/app.yaml`) is unknown whenever the twin would be inside
    code_paths -- or the oracle cannot say -- or would be dispatched to a
    different language arm (round 16): on NTFS / APFS the two spellings are
    one file, and every exact-spelling decision consults the twins through
    one helper, `fold_twins_of`. A twin at any ANCESTOR counts too (round
    17): with `config/` and `Config/` tracked, `Config/app.yaml` is
    `config/app.yaml` there (`fold_twin_dir:<prefix>:<spelling>`). The fold
    key maps U+0131 to `i`, as a simple-uppercase table does. It is canonical
    caseless matching (round 18: the key ends up decomposed, so `ΐ` and `Ϊ́`
    meet) with Default_Ignorable code points dropped first (round 20: ext4
    drops them) and every iota sorted after the marks around it (round 19:
    U+0345 folds to an iota that D145 moves and ext4 and simple uppercase do
    not), so it joins every pair three models join, for names whose code
    points this interpreter's Unicode assigns: D145, ext4/f2fs casefolding as
    e2fsprogs 1.47.2's utf8-12.1 table does it (measured with `e2fsck`, not
    against a kernel), and per-unit simple uppercase, a superset of the
    $UpCase table mkntfs writes. Some names the key cannot judge (rounds 21
    and 22): a component holding a code point the interpreter's Unicode
    leaves unassigned, the only way a later Unicode can fold or normalise
    two names together, or one shaped like an NTFS 8.3 short name
    (`CONFIG~1` can BE `CONFIGURATION`). A path with one on its way, at any
    level, whose directory holds any other entry, or whose directory at any
    level holds one, is unknown outright (`unassigned_codepoint:<name>`,
    `short_name:<name>`, and `undecodable:<name>` for a byte that is not
    UTF-8, round 23). No twin spelling is composed for it. code_paths globs
    go through the same rules (rounds 23 to 26), trimmed as Rust's
    `str::trim` trims them, parsed as globset 0.4.18 parses them and
    matched, as it matches, against a path's bytes. Each alternative also
    matches a path's `fold_key` with its literal runs folded, less the
    combining marks and iotas at an edge a wildcard touches, and every `?`,
    `*` and class widened to `.*`. And in each alternative a component that
    may match a name the key cannot judge makes unknown every path with an
    entry at that level in a directory the tokens before it can match
    (`code_paths_wild:<glob>`). Two checks find one. By its literal
    characters, in the first component between the tokens every match puts
    a `/` in (a literal `/`, `/**`, `/**/` and a leading `**/`, but not a
    later `**/`, which may be empty): unescaped and literal, a `wild_kind`
    name or one ending in a dot or a space; with wildcards, one whose
    literal characters hold `~`, a Cn or a Cs code point, or end in a dot
    or a space. And by 8.3 shape, across the whole alternative as globset
    matches a path: any component the glob can spell as a short name, each
    `?` and class standing for a character it matches or for the `/` that
    ends a component, and each `*`, and the run in a `**` form, for
    characters other than `~` (the next bullet), `/` among them. The same
    holds for every path the resolver walks, from its start directory down,
    so an include or a link read through such a directory is not followed.
    The cost: one such name makes every path beside it, and every path
    under its siblings, unknown. The provenance line names the interpreter's
    Unicode version (`unicode=`).
  * A `*` in a code_paths glob, or the run in a `**` form, is not taken to
    supply the `~` of an 8.3 short name (round 25). A component whose
    short name only a `*` can complete is not judged wild: `CONFIG*/**`
    spells `CONFIG~1/`, and
    `**/*.htm` spells `INDEX~1.HTM`, the short name `index.html` may have.
    On a volume that keeps short names that file is inside such a glob,
    and here it is judged by its own spelling. Taking a `*` to supply the
    `~` would make `*.rs` spell `AB~1.RS`, and every path beside it unknown.
  * Out of the modelled filesystems: compatibility normalisation. ZFS with
    `normalization=formKC` (or `formKD`) makes a full-width letter and its
    ASCII one, or `①` and `1`, a single name. The key joins canonical
    equivalents, what full case folding joins (`ß` and `ss`, `ﬁ` and `fi`),
    U+0131 with `i`, and names that differ only by Default_Ignorable code
    points, iota placement or trailing dots and spaces; it joins no other
    compatibility variant, so `Ａ` and `A` get two keys.
  * Default_Ignorable_Code_Point has no Unicode stability policy (U+180E
    went from Zs to Cf and became ignorable in 6.3). `_DEFAULT_IGNORABLE`
    holds every code point that 13.0, 15.1, 16.0 or 17.0 makes ignorable,
    and from 13.0 through 17.0 none was assigned before it became ignorable.
    A later version may still make an assigned code point ignorable. A newer
    ext4/f2fs table would then drop it while the key keeps it, so names that
    table makes one would get two keys. This is not failed closed: no finite
    category rule covers it, since U+180E came from Zs, and Zs holds U+0020.
  * A reader in ANOTHER file is invisible to a per-file proof (coord's own
    residual, plan risk R2): code that opens a doc file by a computed path, a
    JSON module import, a framework that publishes docstrings. Framework-
    published docs are REGENERATED by the sweep, not exempted (D7).
  * Self-reads are recognised by spelling (round 33). A path rebuilt from
    pieces that spell none of the names above (`"x" + ".js"`, a directory
    listing, `ps` output) is not seen. Neither is a command run by a computed
    name that EVALUATES text (`c=eval; $c '...'`): a computed name counts as
    a reader, not an evaluator. Other evaluators (`bind -x`, PROMPT_COMMAND,
    `PS4`, `source` of a generated file) are not parsed. A script run from
    stdin (`bash < x.sh`, `curl ... | bash`) can read the rest of its own
    text from stdin, and no reader of stdin is refused. Verbose mode turned
    on from outside the script (`bash -v x.sh`, an inherited SHELLOPTS) is
    not seen. A JS/TS self-name reached through an alias (`const m = module;
    m.filename`) or a reflective read (`Reflect.get(module, k)`,
    `Object.values(module)`) is not seen either.
  * Line numbers shift when a comment gains a line; a line-number observer in
    Rust/TS/shell (`line!()`, a stack trace) is not refused here. Nor is a
    diagnostic that prints a source line's TEXT on a failure path (round 35):
    node's uncaught-exception banner and Python's traceback show the line
    that raised, a trailing comment included. A comment sharing a line with
    code that may throw is therefore observable on that path; refusing it
    would make every trailing-comment edit executable in every language, so
    the failure path is declared here instead.
  * A directive is recognised by its spelling; a tool keyed on a comment shape
    not listed below is not.
  * A module's parent is found by NAME (`mod <name>;` in any .rs file, or a
    `#[path]` attribute naming the file); a parent declared under another
    name by a `#[path]` that does not spell the file's basename is not seen.
  * Compile-time readers are recognised by name: a proc macro or a build
    script (`build.rs`) that reads a non-code file by its own logic, and a
    macro or derive imported under another name (`use clap::Parser as Debug`)
    is not seen. A build script reading a file is the R2 residual above.
  * rustdoc lints over prose (`broken_intra_doc_links` under `#![deny]`) can
    fail `cargo doc` on a prose edit; that is not refused here.
  * A doctest's text is found by reading its CONTAINERS (doc comments, doc
    attributes, doc-capable literals, included files). Doc text a proc
    macro or a `concat!` assembles is refused by rule 1a / 1b, not read.
  * A text doctest is recognised by a `>>>` prompt or a Sphinx doctest
    directive, in UTF-8 or (when a BOM or NULs suggest it) UTF-16/32; a text
    a runner reads in another encoding (`--doctest-encoding=latin-1` with no
    such marker in the bytes) is not. Other executing directives (`.. ipython::`, `.. plot::`,
    `.. jupyter-execute::`, a custom collector) run at documentation build
    time and are not recognised here.
"""

from __future__ import annotations

import argparse
import ast
import difflib
import html.entities
import http.client
import json
import os
import posixpath
import re
import shutil
import subprocess
import sys
import tempfile
import unicodedata
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from typing import Optional

for _stream in (sys.stdout, sys.stderr):
    # A path or a coord note can carry characters the Windows console codepage
    # cannot encode; without this the verdict would be replaced by a traceback
    # AFTER the work is done.
    _reconfigure = getattr(_stream, "reconfigure", None)
    if _reconfigure is None:
        continue
    try:
        _reconfigure(encoding="utf-8", errors="backslashreplace")
    except (ValueError, OSError):
        pass

HERE = os.path.dirname(os.path.abspath(__file__))
LIB = os.path.join(HERE, "lib")
sys.path.insert(0, LIB)

try:
    from git_scope import git_scope_strip  # noqa: E402  (lib/ beside this file)
    from bash_resolve import resolve as resolve_bash  # noqa: E402
except ImportError as _exc:  # a broken install: lib/ did not travel with the skill
    print("exec-equivalence: cannot import its lib/ helpers (%s) -- no verdict" % _exc, file=sys.stderr)
    sys.exit(2)

EXIT_OK, EXIT_EXECUTABLE, EXIT_UNKNOWN, EXIT_USAGE = 0, 4, 5, 2

NON_EXECUTABLE, EXECUTABLE, UNKNOWN = "non_executable", "executable", "unknown"

#: The pinned grammar pack. 0.13.0 BUNDLES its grammars in the wheel; the 1.x
#: line downloads grammar libraries at first use, which would make a verifier
#: fetch native code from the network at check time. A different installed
#: version is `unknown`: the node-type names the rules below read are this
#: version's. CI installs it with its own dependencies pinned beside it: bump
#: `deps=` in the grammar-pack step of .github/workflows/lint-frontmatter.yml
#: together with this pin.
TREE_SITTER_PACK = "tree-sitter-language-pack"
TREE_SITTER_PACK_PIN = "0.13.0"

DEFAULT_COORD_URL = "https://coord.qontinui.io"
PYTHON_ROUTE = "/coord/exec-equivalence/python"
SCOPE_ROUTE = "/coord/post-merge-followup-scope"
#: coord caps each side of the Python route at 1 MiB and answers 413 above it.
SIDE_CAP_BYTES = 1024 * 1024
HTTP_TIMEOUT_SECS = 30
STAGE_TIMEOUT_SECS = 300

EXT_LANG = {
    ".py": "python", ".pyi": "python", ".pyw": "python",
    ".rs": "rust",
    ".ts": "typescript", ".mts": "typescript", ".cts": "typescript",
    ".tsx": "tsx",
    ".js": "javascript", ".mjs": "javascript", ".cjs": "javascript", ".jsx": "javascript",
    ".sh": "bash", ".bash": "bash",
}
SHEBANG_LANG = {
    "sh": "bash", "bash": "bash", "dash": "bash",
    "python": "python", "python3": "python",
    "node": "javascript", "nodejs": "javascript",
}
TREE_SITTER_LANGS = ("rust", "typescript", "tsx", "javascript", "bash")
JS_FAMILY = ("typescript", "tsx", "javascript")

#: Inside code_paths these have no inert sub-language: every byte is the
#: program (a prompt) or the data (JSON has no comments), so any change is
#: executable. Every other file type without a language arm is `unknown`.
WHOLLY_SIGNIFICANT_EXTS = (".md", ".markdown", ".mdx", ".json", ".csv", ".tsv")

MODE_SYMLINK = "120000"
MODE_GITLINK = "160000"
NULL_SHA = "0" * 40


class UsageError(Exception):
    """A usage, git or internal error: no verdict (exit 2)."""


# ---------------------------------------------------------------------------
# git
# ---------------------------------------------------------------------------

#: On every git call (round 28's F5 and F6): replace refs never stand in for
#: an object, and no `attr.tree` makes git read attributes from another tree.
#: GIT_ATTR_SOURCE, which outranks attr.tree, is stripped in `run`.
GIT_PIN = ("--no-replace-objects", "-c", "attr.tree=")
ATTR_ENV = ("GIT_ATTR_SOURCE",)


def git(root: str, *args: str, ok_codes=(0,), input: Optional[bytes] = None, env: Optional[dict] = None,
        glob_pathspecs: bool = False) -> bytes:
    """Run `git -C <root> <args>` and return stdout bytes; a non-ok exit is a
    UsageError naming the command, never an empty answer. Every pathspec is
    LITERAL (round 27's F1): a repository path such as `:notes.txt` or
    `:(literal)x` is a name, never pathspec magic. An inherited
    GIT_*_PATHSPECS, which would change that or make git refuse the flag, is
    gone before any call (`git_scope_strip` in `run`). Only a caller that
    passes a glob of its own (`glob_pathspecs`, git grep's `*.rs`) opts out."""
    cmd = ["git", "-C", root, *GIT_PIN, "-c", "core.quotepath=off"]
    if not glob_pathspecs:
        cmd.append("--literal-pathspecs")
    cmd += args
    try:
        proc = subprocess.run(cmd, input=input, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except OSError as exc:
        raise UsageError("cannot run git: %s" % exc) from exc
    if proc.returncode not in ok_codes:
        err = proc.stderr.decode("utf-8", "replace").strip().splitlines()
        raise UsageError("git %s failed (exit %d): %s" % (" ".join(args[:3]), proc.returncode, err[-1] if err else ""))
    return proc.stdout


def resolve_commit(root: str, ref: str) -> str:
    out = git(root, "rev-parse", "--verify", "--quiet", ref + "^{commit}", ok_codes=(0, 1))
    sha = out.decode("ascii", "replace").strip()
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise UsageError("%r does not name a commit in %s" % (ref, root))
    return sha


@dataclass
class Change:
    path: str
    status: str            # A M D T; U, unmerged, in the index view (R and C never appear: --no-renames)
    old_mode: str
    new_mode: str
    old_sha: str
    new_sha: str           # NULL_SHA where the working tree, not a blob, holds the head
    untracked: bool = False
    on_disk: bool = False  # working tree: head is read off disk, not rebuilt by git
    base: Optional[bytes] = None
    head: Optional[bytes] = None
    lang: Optional[str] = None
    verdict: str = UNKNOWN
    reason: str = "not evaluated"
    arm: str = "none"


def parse_raw_diff(out: bytes) -> list:
    """`git diff --raw -z` records: `:<om> <nm> <osha> <nsha> <st>\\0<path>\\0`."""
    changes = []
    fields = out.split(b"\0")
    i = 0
    while i < len(fields):
        head = fields[i]
        if not head:
            i += 1
            continue
        if not head.startswith(b":"):
            raise UsageError("unexpected git diff --raw record %r" % head[:60])
        parts = head[1:].decode("ascii", "replace").split()
        if len(parts) != 5 or i + 1 >= len(fields):
            raise UsageError("malformed git diff --raw record %r" % head[:60])
        path = fields[i + 1].decode("utf-8", "surrogateescape")
        status = parts[4][0]
        changes.append(Change(path=path, status=status, old_mode=parts[0], new_mode=parts[1],
                              old_sha=parts[2], new_sha=parts[3]))
        i += 2
    return changes


def list_changes(root: str, base: str, head: Optional[str]) -> list:
    args = ["diff", "--raw", "-z", "--no-renames", "--no-ext-diff", "--no-textconv",
            "--abbrev=40", "--ignore-submodules=none", base]
    if head:
        args.append(head)
    args.append("--")
    changes = parse_raw_diff(git(root, *args))
    if head is None:
        others = [raw.decode("utf-8", "surrogateescape")
                  for raw in git(root, "ls-files", "-z", "--others", "--exclude-standard").split(b"\0") if raw]
        listed = {p.rstrip("/") for p in others}
        changes += [r for r in (reconcile_deleted(root, c, listed) for c in changes if c.status == "D") if r]
        seen = {c.path for c in changes}
        for path in others:
            if path.endswith("/"):
                # A nested repository at a path neither side records: git
                # lists it as one `dir/` entry and does not look inside (round
                # 23's F1). It is a submodule the checker cannot see into, so
                # it is judged as a gitlink, and round 10's B4 rule reads it
                # unknown outside code_paths. One at a path the diff records
                # as deleted is `reconcile_deleted`'s (round 24). No other
                # --others shape ends in `/`: an empty or an ignored directory
                # is not listed, and a link to a directory is one entry
                # without the slash.
                path = path.rstrip("/")
                if path not in seen:
                    changes.append(Change(path=path, status="A", old_mode="000000", new_mode=MODE_GITLINK,
                                          old_sha=NULL_SHA, new_sha=NULL_SHA, untracked=True))
                continue
            if path in seen:
                continue
            full = os.path.join(root, *path.split("/"))
            mode = MODE_SYMLINK if os.path.islink(full) else "100755" if os.access(full, os.X_OK) else "100644"
            changes.append(Change(path=path, status="A", old_mode="000000", new_mode=mode,
                                  old_sha=NULL_SHA, new_sha=NULL_SHA, untracked=True))
        # Everything above is what `git add -A` would commit. A commit of the
        # index records the staged side, which the working tree may no longer
        # hold (round 26's F1): a file staged and then edited, or staged and
        # then removed. Each such side is judged too, from its blob, and
        # `keep_worse` keeps the worse reading.
        sides = {(c.path, c.status, c.new_mode, c.new_sha) for c in changes}
        for s in parse_raw_diff(git(root, "diff", "--cached", *args[1:])):
            if (s.path, s.status, s.new_mode, s.new_sha) not in sides:   # the index holds another side
                changes.append(s)
    changes.sort(key=lambda c: c.path)
    return changes


def reconcile_deleted(root: str, change: Change, listed: set) -> Optional[Change]:
    """Working-tree mode: what may replace a path the diff records as deleted,
    or None (rounds 24 and 25). The deletion stands beside it, and
    `keep_worse` reports the worse of the two, since a commit of the index
    records the deletion while `git add -A` records the replacement. A file
    or a link replaces it only where --others lists it, so one ignored
    (round 25's F1), which `git add -A` would leave out, does not; it is
    read off disk (`M`, or `T` when its type changes). A nested repository
    is a gitlink (`T`), listed or not: one with no commit is listed as
    `dir/` once `git rm --cached` has taken the path out of the index, and
    nowhere while the index still holds it (round 26's F4).
    Nothing replaces a path under a link, which git does not follow and
    `os.path` does, or one where a plain directory stands: --others lists
    what is in it."""
    parts = change.path.split("/")
    if any(os.path.islink(os.path.join(root, *parts[:i])) for i in range(1, len(parts))):
        return None
    full = os.path.join(root, *parts)
    if os.path.islink(full):
        mode = MODE_SYMLINK
    elif os.path.isdir(full):
        if not os.path.lexists(os.path.join(full, ".git")):
            return None
        mode = MODE_GITLINK
    elif os.path.isfile(full):
        mode = "100755" if os.access(full, os.X_OK) else "100644"
    else:
        return None
    if mode == MODE_GITLINK:
        return Change(path=change.path, status="T", old_mode=change.old_mode, new_mode=mode,
                      old_sha=change.old_sha, new_sha=NULL_SHA, untracked=True)
    if change.path not in listed:
        return None
    regular = ("100644", "100755")
    status = "M" if (change.old_mode in regular) == (mode in regular) else "T"
    return Change(path=change.path, status=status, old_mode=change.old_mode, new_mode=mode,
                  old_sha=change.old_sha, new_sha=NULL_SHA, on_disk=True)


def keep_worse(changes: list) -> list:
    """`changes` with each path reported once: a deletion and what may
    replace it (`reconcile_deleted`) keep the worse reading, executable over
    unknown over non_executable, and the deletion's on a tie."""
    rank = {NON_EXECUTABLE: 0, UNKNOWN: 1, EXECUTABLE: 2}
    kept = []
    for c in changes:
        if kept and kept[-1].path == c.path:
            if rank[c.verdict] > rank[kept[-1].verdict]:
                kept[-1] = c
            continue
        kept.append(c)
    return kept


def read_link(root: str, path: str) -> Optional[bytes]:
    """A working-tree link's target, as git stores it, or None if unreadable."""
    try:
        return os.readlink(os.path.join(root, *path.split("/"))).encode("utf-8", "surrogateescape")
    except OSError:
        return None


def clean_blobs(root: str, paths: list) -> dict:
    """{path: bytes}: each working-tree file whole, as `git add` would store
    it (round 27's F2). `git hash-object --path` applies what git itself
    applies on the way in, and writes into a throwaway object directory, so
    the repository's own is never written. A path with a checkout attribute
    never gets here (`checkout_attribute_reasons`, round 28), so what is
    left is config's end-of-line conversion (core.autocrlf). A file git
    cannot read or convert is absent."""
    if not paths:
        return {}
    tmp = tempfile.mkdtemp(prefix="exec-eq-objects-")
    try:
        env = dict(os.environ, GIT_OBJECT_DIRECTORY=tmp)
        shas = {}
        for p in paths:
            try:
                with open(os.path.join(root, *p.split("/")), "rb") as fh:
                    data = fh.read()
                out = git(root, "hash-object", "-w", "--stdin", "--path=" + p, input=data, env=env)
            except (OSError, UsageError):
                continue
            shas[p] = out.decode("ascii", "replace").strip()
        blobs = read_blobs(root, list(shas.values()), env=env)
        return {p: blobs[sha] for p, sha in shas.items()}
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


#: The attributes gitattributes(5) gives that change the bytes a checkout
#: writes: a filter driver, end-of-line conversion (`text`, `eol` and the
#: legacy `crlf`), re-encoding, and `$Id$` expansion. `binary` is a macro
#: for `-text`, which converts nothing.
CHECKOUT_ATTRS = ("filter", "text", "eol", "crlf", "working-tree-encoding", "ident")


def read_attributes(root: str, paths: list, rev: Optional[str] = None, cached: bool = False) -> dict:
    """{path: {attr: value}} for CHECKOUT_ATTRS, as git reads attributes for
    the commit `rev`, for the real index (`cached`), or for the working tree.
    A commit's are read through a throwaway index built from its tree
    (`check-attr --cached`), which any git answers and which no attr.tree or
    GIT_ATTR_SOURCE redirects; `check-attr --source` needs 2.40. Paths go in
    on stdin, as names, never as pathspecs."""
    if not paths:
        return {}
    tmp, env, args = None, None, ["check-attr", "-z", "--stdin"]
    try:
        if rev:
            tmp = tempfile.mkdtemp(prefix="exec-eq-attr-")
            env = dict(os.environ, GIT_INDEX_FILE=os.path.join(tmp, "index"))
            git(root, "read-tree", rev, env=env)
            cached = True
        if cached:
            args.append("--cached")
        out = git(root, *args, *CHECKOUT_ATTRS, env=env,
                  input=b"".join(p.encode("utf-8", "surrogateescape") + b"\0" for p in paths))
    finally:
        if tmp:
            shutil.rmtree(tmp, ignore_errors=True)
    fields, found = out.split(b"\0"), {}
    for i in range(0, len(fields) - 2, 3):
        path = fields[i].decode("utf-8", "surrogateescape")
        found.setdefault(path, {})[fields[i + 1].decode("utf-8", "replace")] = fields[i + 2].decode("utf-8", "replace")
    return found


def _holds_attribute_lines(path: str) -> bool:
    """True when `path` is a file holding a line that is neither blank nor a
    comment, or exists and cannot be read (it may hold one). A line is what
    git's attribute parser reads as one: split at LF only, and blank or a
    comment only after exactly git's blank set, ` \t\r\n` (round 31's F1).
    `bytes.strip()` also drops VT and FF, so a line `\v#x.txt -filter`,
    which git applies, would read as a comment."""
    if not os.path.lexists(path):
        return False
    try:
        with open(path, "rb") as fh:
            lines = fh.read().split(b"\n")
    except OSError:
        return True
    for ln in lines:
        s = ln.lstrip(b" \t\r\n")
        if s and not s.startswith(b"#"):
            return True
    return False


def _git_path_line(out: bytes) -> str:
    """A path git printed on one line, less only that line's LF (round 31):
    a path may end in a space, a tab or a VT that `strip()` would drop, and
    git reads the file at the path as written."""
    if out.endswith(b"\n"):
        out = out[:-1]
    return out.decode("utf-8", "surrogateescape")


def _git_env_true(name: str) -> bool:
    """Whether `name` turns a git switch on, close to git's boolean grammar
    but not identical to it. Exactly what is parsed: the value, stripped of
    whitespace and lowercased, is false when empty or one of 0, false, no,
    off; true when one of 1, true, yes, on; otherwise true when Python's
    `int()` reads a non-zero integer from it, and false in every other case.
    The two grammars disagree in both directions (round 30), and each
    mismatch fails closed:
      * read true here, refused by git (` true`, `1 `, `1_0`, `١`): git dies
        on the value (`bad boolean environment value`, exit 128) in the first
        call that reads attributes -- `git diff --raw` itself against the
        working tree, `git check-attr` between two commits -- so the run is
        a usage error, exit 2, once such a call is reached (an empty diff
        exits 0, and another local attributes source found first exits 5);
      * read false here, refused by git (` `, `maybe`, `0_0`): against the
        working tree `git diff --raw` dies first, exit 2, as above; between
        two commits `git var GIT_ATTR_SYSTEM` dies (exit 128), so every path
        is `local_attributes_source:GIT_ATTR_SYSTEM:unresolved`, exit 5;
      * read false here, true to git (`0x1`, `1k`): the system file is then
        looked up, and `git var GIT_ATTR_SYSTEM` answers nothing while git
        holds the switch on (exit 1), so every path is
        `local_attributes_source:GIT_ATTR_SYSTEM:unresolved`.
    None of these outcomes judges a file from the misread value."""
    v = os.environ.get(name, "").strip().lower()
    if v in ("", "0", "false", "no", "off"):
        return False
    if v in ("1", "true", "yes", "on"):
        return True
    try:
        return int(v) != 0
    except ValueError:
        return False


def local_attribute_sources(root: str) -> list:
    """The attribute files a diff cannot show that hold a line:
    $GIT_DIR/info/attributes; core.attributesFile or, unset, its default
    $XDG_CONFIG_HOME/git/attributes ($HOME/.config/git/attributes); and the
    system file $(prefix)/etc/gitattributes (round 29's F1), which git reads
    unless GIT_ATTR_NOSYSTEM is true. Each adds to or outranks the tracked
    `.gitattributes` on this machine only -- a macro the system file defines
    can unset a filter a tracked line names before it -- so a verdict read
    through one is not the commit's. The system file's path is compiled into
    git and only `git var GIT_ATTR_SYSTEM` (git 2.42) names it; an older git
    leaves it unknown, and that is a source too."""
    info = _git_path_line(git(root, "rev-parse", "--git-path", "info/attributes"))
    cfg = _git_path_line(git(root, "config", "--path", "--get", "core.attributesFile", ok_codes=(0, 1)))
    if not cfg:
        xdg = os.environ.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config")
        cfg = os.path.join(xdg, "git", "attributes")
    found, files = [], [info, cfg]
    if not _git_env_true("GIT_ATTR_NOSYSTEM"):
        try:
            system = _git_path_line(git(root, "var", "GIT_ATTR_SYSTEM"))
        except UsageError:
            system = ""
        if system:
            files.append(system)
        else:
            found.append("GIT_ATTR_SYSTEM:unresolved")
    for f in files:
        full = f if os.path.isabs(f) else os.path.join(root, f)
        if _holds_attribute_lines(full):
            found.append(f)
    return found


def attribute_source_change(changes: list) -> Optional[str]:
    """The first changed path that is a `.gitattributes` at any depth, by any
    spelling a modelled filesystem opens as one, or None."""
    key = fold_key(".gitattributes")
    for c in changes:
        if fold_key(posixpath.basename(c.path)) == key:
            return c.path
    return None


def checkout_attribute_reasons(root: str, changes: list, base_sha: str, head_sha: Optional[str]) -> dict:
    """{path: reason} for every change git attributes make unknown (round
    28: attributes change what a checkout holds, so no content rule can
    judge a path they touch). A non-empty local attributes source, or a
    changed `.gitattributes`, makes every path unknown: either can change the
    attributes of any path, changed or not. Otherwise a path is unknown when
    a checkout attribute is set for it on any side, or reads differently
    between sides. The sides are the two commits or, against the working
    tree, the base commit, the working tree (what `git add -A` reads) and
    the real index (what a commit of the index carries, round 28's F4)."""
    if not changes:
        return {}
    local = local_attribute_sources(root)
    if local:
        return {c.path: "local_attributes_source:" + local[0] for c in changes}
    source = attribute_source_change(changes)
    if source:
        return {c.path: "attributes_source_changed:" + source for c in changes}
    paths = sorted({c.path for c in changes})
    sides = [read_attributes(root, paths, rev=base_sha)]
    if head_sha:
        sides.append(read_attributes(root, paths, rev=head_sha))
    else:
        sides.append(read_attributes(root, paths))
        sides.append(read_attributes(root, paths, cached=True))
    reasons = {}
    for p in paths:
        for a in CHECKOUT_ATTRS:
            values = [side.get(p, {}).get(a, "unspecified") for side in sides]
            hit = [v for v in values if v not in ("unspecified", "unset")]
            if hit:
                reasons[p] = "checkout_attribute:%s=%s" % (a, hit[0])
                break
            if len(set(values)) > 1:
                reasons[p] = "checkout_attribute_differs:" + a
                break
    return reasons


def read_blobs(root: str, shas: list, env: Optional[dict] = None) -> dict:
    """{sha: bytes} for every blob, through ONE `git cat-file --batch` (a
    process per blob is most of a run's cost where spawning is slow). A blob
    git cannot produce is a git error, never an empty side."""
    wanted = sorted(set(shas))
    if not wanted:
        return {}
    cmd = ["git", "-C", root, *GIT_PIN, "cat-file", "--batch"]
    try:
        proc = subprocess.run(cmd, input=("\n".join(wanted) + "\n").encode("ascii"), env=env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except OSError as exc:
        raise UsageError("cannot run git: %s" % exc) from exc
    if proc.returncode != 0:
        raise UsageError("git cat-file --batch failed (exit %d)" % proc.returncode)
    out, pos, blobs = proc.stdout, 0, {}
    for sha in wanted:
        eol = out.find(b"\n", pos)
        header = out[pos:eol].decode("ascii", "replace").split() if eol >= 0 else []
        if len(header) != 3 or header[0] != sha or header[1] != "blob":
            raise UsageError("git cat-file --batch could not produce blob %s (%s)" % (sha, " ".join(header) or "no answer"))
        size = int(header[2])
        start = eol + 1
        blobs[sha] = out[start:start + size]
        if len(blobs[sha]) != size or out[start + size:start + size + 1] != b"\n":
            raise UsageError("git cat-file --batch answered a truncated blob %s" % sha)
        pos = start + size + 1
    return blobs


def reconstruct_worktree(root: str, base_sha: str, path: str, base_bytes: bytes) -> Optional[bytes]:
    """The working-tree side of <path> AS GIT WOULD STORE IT (clean filters and
    end-of-line conversion applied), rebuilt from a full-context `git diff`.
    Reading the file off disk instead would compare a CRLF checkout against an
    LF blob. Returns None when the rebuilt BASE side does not reproduce the
    base blob byte for byte -- the reconstruction is then not trusted."""
    full = os.path.join(root, *path.split("/"))
    try:
        with open(full, "rb") as fh:
            disk_lines = fh.read().count(b"\n")
    except OSError:
        disk_lines = 0
    ctx = max(base_bytes.count(b"\n"), disk_lines) + 3
    out = git(root, "-c", "diff.suppressBlankEmpty=false", "-c", "diff.noprefix=false",
              "diff", "--no-color", "--no-ext-diff", "--no-textconv", "--text", "--no-renames",
              "-U%d" % ctx, base_sha, "--", path)   # literal (`git`): `:x` is a name, never magic
    lines = out.split(b"\n")
    if lines and lines[-1] == b"":
        lines.pop()
    try:
        start = next(i for i, ln in enumerate(lines) if ln.startswith(b"@@ "))
    except StopIteration:
        return base_bytes  # git saw no content change (e.g. a mode-only change)
    old, new = [], []      # lists of [bytes, has_newline]
    hunks = 0
    last = None
    for ln in lines[start:]:
        if ln.startswith(b"@@ "):
            hunks += 1
            if hunks > 1:
                return None  # context too small to cover the file: not trusted
            continue
        tag, body = ln[:1], ln[1:]
        if tag == b" ":
            old.append([body, True])
            new.append([body, True])
            last = "both"
        elif tag == b"-":
            old.append([body, True])
            last = "old"
        elif tag == b"+":
            new.append([body, True])
            last = "new"
        elif tag == b"\\":
            if last in ("both", "old") and old:
                old[-1][1] = False
            if last in ("both", "new") and new:
                new[-1][1] = False
        else:
            return None
    rebuilt_old = b"".join(b + (b"\n" if nl else b"") for b, nl in old)
    if rebuilt_old != base_bytes:
        return None
    return b"".join(b + (b"\n" if nl else b"") for b, nl in new)


def origin_slug(root: str) -> Optional[str]:
    """owner/name from a GitHub origin url, or None. Never guesses."""
    out = git(root, "config", "--get", "remote.origin.url", ok_codes=(0, 1))
    url = out.decode("utf-8", "replace").strip()
    m = re.match(r"^(?:https?://github\.com/|ssh://git@github\.com/|git@github\.com:)(.+?)/?$", url)
    if not m:
        return None
    slug = m.group(1)
    if slug.endswith(".git"):
        slug = slug[:-4]
    if not re.fullmatch(r"[A-Za-z0-9._-]+/[A-Za-z0-9._-]+", slug):
        return None
    return slug


# ---------------------------------------------------------------------------
# code_paths -- globset defaults, mirrored
# ---------------------------------------------------------------------------

class GlobError(ValueError):
    pass


def glob_tokens(pat: str) -> list:
    """`pat` parsed as globset 0.4.18's `Glob::new` parses it (the version
    coord's Cargo.lock pins) under coord's options: backslash escapes on,
    empty alternatives off, an unclosed class an error (round 25's F2 and
    F3). It is a port of that parser, branch for branch, so its `**` rules
    come along, quirks and all: `**/**` is one `**/`, `a/**/**` one `a/**`,
    and a `**` beside anything but a separator is two `*`. A token is
    ("lit", char), ("any",), ("star",), ("rprefix",) for `**/`,
    ("rsuffix",) for `/**`, ("rzom",) for `/**/`, ("class", negated,
    ranges) or ("alt", [branch, ...]). Raises GlobError wherever globset
    returns an error."""
    pos, prev, cur = 0, None, None
    branches, alt_starts = [[]], []

    def bump():
        nonlocal pos, prev, cur
        prev, cur = cur, (pat[pos] if pos < len(pat) else None)
        pos += cur is not None
        return cur

    def peek():
        return pat[pos] if pos < len(pat) else None

    def parse_star():
        before = prev
        if peek() != "*":
            branches[-1].append(("star",))
            return
        bump()
        if not branches[-1]:
            if peek() not in (None, "/"):
                branches[-1] += [("star",), ("star",)]
            else:
                branches[-1].append(("rprefix",))
                bump()
            return
        if before != "/" and (len(branches) <= 1 or before not in (",", "{")):
            branches[-1] += [("star",), ("star",)]
            return
        nxt = peek()
        if nxt is None or nxt == "/":
            bump()
            suffix = nxt is None
        elif nxt in ",}" and len(branches) >= 2:
            suffix = True
        else:
            branches[-1] += [("star",), ("star",)]
            return
        last = branches[-1].pop()   # the separator, or a recursive form this `**` joins
        branches[-1].append(last if last in (("rprefix",), ("rsuffix",)) else ("rsuffix",) if suffix else ("rzom",))

    def parse_class():
        negated = peek() in ("!", "^")
        if negated:
            bump()
        ranges, first, in_range = [], True, False
        while True:
            c = bump()
            if c is None:
                raise GlobError("unclosed class")
            if c == "]" and not first:
                break
            if c == "-" and not first and not in_range:
                in_range = True
            elif in_range:
                if c < ranges[-1][0]:
                    raise GlobError("invalid range %r-%r" % (ranges[-1][0], c))
                ranges[-1] = (ranges[-1][0], c)
                in_range = False
            else:
                ranges.append((c, c))
            first = False
        if in_range:
            ranges.append(("-", "-"))
        return ("class", negated, tuple(ranges))

    while bump() is not None:
        c = cur
        if c == "?":
            branches[-1].append(("any",))
        elif c == "*":
            parse_star()
        elif c == "[":
            branches[-1].append(parse_class())
        elif c == "{":
            alt_starts.append(len(branches))
            branches.append([])
        elif c == "}":
            if not alt_starts:
                raise GlobError("unopened alternates")
            start = alt_starts.pop()
            alt = ("alt", branches[start:])
            del branches[start:]
            branches[-1].append(alt)
        elif c == "," and alt_starts:
            branches.append([])
        elif c == "\\":
            if bump() is None:
                raise GlobError("dangling escape")
            branches[-1].append(("lit", cur))
        else:
            branches[-1].append(("lit", c))
    if len(branches) > 1:
        raise GlobError("unclosed alternates")
    return branches[0]


def _glob_bytes(ch: str) -> str:
    """`ch`'s UTF-8 bytes as a literal in a bytes regex, as globset writes it."""
    try:
        data = ch.encode("utf-8")
    except UnicodeEncodeError as exc:
        raise GlobError("not UTF-8") from exc
    return "".join(re.escape(chr(b)) if b < 0x80 else "\\x%02x" % b for b in data)


def glob_regex(tokens: list) -> str:
    """globset's regex for `tokens`. It is over BYTES, as globset's `(?-u)`
    makes it: `?` and a class match one byte, so `??` and `[!/][!/]` match
    `é`. Compile it as bytes with re.DOTALL (globset sets
    dot_matches_new_line) and fullmatch a path's bytes."""
    out = []
    for t in tokens:
        kind = t[0]
        if kind == "lit":
            out.append(_glob_bytes(t[1]))
        elif kind == "any":
            out.append(".")
        elif kind == "star":
            out.append(".*")
        elif kind == "rprefix":
            out.append("(?:/?|.*/)")
        elif kind == "rsuffix":
            out.append("/.*")
        elif kind == "rzom":
            out.append("(?:/|/.*/)")
        elif kind == "class":
            out.append("[%s%s]" % ("^" if t[1] else "", "".join(
                _glob_bytes(lo) + ("" if lo == hi else "-" + _glob_bytes(hi)) for lo, hi in t[2])))
        else:
            parts = [p for p in (glob_regex(b) for b in t[1]) if p]
            if parts:
                out.append("(?:%s)" % "|".join(parts))
    return "".join(out)


def glob_matcher(tokens: list):
    """The exact matcher: globset's regex, `**` alone (or `**/`, which parses
    to it) matching every path, as globset special-cases it."""
    return re.compile((".*" if tokens == [("rprefix",)] else glob_regex(tokens)).encode("ascii"), re.DOTALL)


def glob_alternatives(tokens: list) -> list:
    """The flat token lists `tokens` stands for: one per choice of branch at
    each alternation, leaving out a branch whose regex is empty as globset
    does. Past 1024 it raises GlobError rather than fold a fraction."""
    flats = [[]]
    for t in tokens:
        if t[0] == "alt":
            choices = [f for b in t[1] if glob_regex(b) for f in glob_alternatives(b)] or [[]]
        else:
            choices = [[t]]
        flats = [f + c for f in flats for c in choices]
        if len(flats) > 1024:
            raise GlobError("more than 1024 alternatives to fold")
    return flats


def _fold_mark(ch: str) -> bool:
    """A character `fold_key` may reorder with its neighbours."""
    return ch == _IOTA or bool(unicodedata.combining(ch))


def folded_glob_regex(flat: list) -> str:
    """A regex, matched with re.DOTALL against a `fold_key`, that holds the
    key of every path the flat token list `flat` matches (rounds 24 and 25).
    Folding the glob's TEXT is wrong: one code point may fold to two (`ß`,
    an NFD `é`) or to none (an ignorable), and a negated class or a range
    across case excludes the fold of a member it admitted. So only the
    literal runs are folded, and `?`, `*` and every class become `.*`. The
    recursive forms read as globset reads them, except that a `**/` past the
    start reads `.*`. Where a `.*` touches a literal run, the run loses the
    combining marks and iotas at that edge (round 25's F5): `fold_key` sorts
    them with the marks the wildcard took, so `a*◌̖/x` holds `A◌́◌̖/x` and
    `a◌̖◌́/x`. A run's last dots and spaces go as `fold_key` drops them,
    since a `/`, the end or a `.*` follows it. It cannot fail to compile."""
    segs = []
    for k, t in enumerate(flat):
        kind = t[0]
        if kind == "lit":
            if segs and isinstance(segs[-1], list):
                segs[-1].append(t[1])
            else:
                segs.append([t[1]])
        elif kind in ("any", "class"):
            segs.append(".*")   # ? and a class: one byte, of a code point that may fold to two or to none
        elif kind == "star" or (kind == "rprefix" and k):
            segs.append(".*")
        elif kind == "rprefix":
            segs.append("(?:/?|.*/)")
        elif kind == "rsuffix":
            segs.append("/.*")
        else:
            segs.append("(?:/|/.*/)")
    out = []
    for k, seg in enumerate(segs):
        if isinstance(seg, list):
            key = fold_key("".join(seg))
            i, j = 0, len(key)
            if k and segs[k - 1] in (".*", "/.*"):
                while i < j and _fold_mark(key[i]):
                    i += 1
            if k + 1 < len(segs) and segs[k + 1] == ".*":
                while j > i and _fold_mark(key[j - 1]):
                    j -= 1
            seg = re.escape(key[i:j])
        if seg and not (seg == ".*" and out and out[-1] == ".*"):
            out.append(seg)
    return "".join(out)


def glob_components(flat: list) -> list:
    """(start, tokens) for each component of the flat token list `flat`: the
    runs between the tokens every match puts a `/` in. A literal `/` is one
    however it was written, and so are `/**`, `/**/` and a leading `**/`.
    A `**/` past the start is not: its `/?` may be empty, gluing what is
    either side of it into one name (round 26's F2). Nor is a `/` a class
    holds."""
    comps, start = [], 0
    for k, t in enumerate(flat + [("lit", "/")]):
        if t == ("lit", "/") or t[0] in ("rsuffix", "rzom") or (t[0] == "rprefix" and not k):
            if k > start:
                comps.append((start, flat[start:k]))
            start = k + 1
    return comps


#: `_SHORT_NAME` after `rstrip(". ")`, as an automaton over kinds of
#: character (round 25's F4): `~`, `0` for a digit, `.`, a space, `/`, and
#: `a` for any other. Its states: `start`, `b1`-`b6` a base of that length,
#: `t` the `~`, `d` the digits, `x0`-`x3` the extension, and `z` the dots and
#: spaces Windows drops.
_SN_KINDS = frozenset("~0. /a")
_SN_NAME = frozenset("~0 a")   # [^./]
_SN_ACCEPT = frozenset(["d", "x1", "x2", "x3", "z"])


def _sn_kind(ch: str) -> str:
    return ch if ch in "~. /" else "0" if ch in "0123456789" else "a"


def _sn_next(state: str, kind: str) -> set:
    nxt = set()
    if kind in _SN_NAME and state in ("start", "b1", "b2", "b3", "b4", "b5"):
        nxt.add("b1" if state == "start" else "b%d" % (int(state[1]) + 1))
    if kind == "~" and state[0] == "b":
        nxt.add("t")
    if kind == "0" and state in ("t", "d"):
        nxt.add("d")
    if kind == "." and state == "d":
        nxt.add("x0")
    if kind in _SN_NAME and state in ("x0", "x1", "x2"):
        nxt.add("x%d" % (int(state[1]) + 1))
    if kind in (".", " ") and state in _SN_ACCEPT:
        nxt.add("z")
    return nxt


def _token_kinds(t) -> frozenset:
    """The kinds of character a component token may stand for: a literal
    its own; a class those of every byte it matches; `?` any; `*` any but
    `~`."""
    if t[0] == "lit":
        return frozenset(_sn_kind(t[1]))
    if t[0] == "class":
        cls = re.compile(glob_regex([t]).encode("ascii"), re.DOTALL)
        return frozenset(_sn_kind(chr(b)) for b in range(256) if cls.fullmatch(bytes([b])))
    return _SN_KINDS if t[0] == "any" else _SN_KINDS - {"~"}


def _sn_step(confs: set, kinds) -> set:
    return {(n, start) for s, start in confs for k in kinds for n in _sn_next(s, k)}


def _sn_closure(confs: set, kinds) -> set:
    while True:
        more = _sn_step(confs, kinds) - confs
        if not more:
            return confs
        confs = confs | more


def glob_short_name_start(flat: list) -> Optional[int]:
    """Where, in the flat token list `flat`, the first component that can
    spell a name `_SHORT_NAME` takes starts (its parent is `flat[:start]`),
    or None (rounds 25 and 26). The automaton runs across the whole
    alternative, as globset matches a path, with the start of the current
    component beside each state. Each `?` and class stands for a character
    it matches, for part of one (a byte of `é`), or, where it can be one,
    for the `/` that ends a component; each `*`, and the run in a `**`
    form, for characters other than `~`, `/` among them (round 26's F2:
    `src?CONFIG?1/**` and `src*CONFIG?1/**` spell `src/CONFIG~1/`). A `/`
    ends the component, which is a hit where a state accepts there, and
    starts the next at `start`. A `**/` past the start may be empty, so
    states run on across it too (`a{**/CONF?1.YAM,x}` spells `aCONF~1.YAM`),
    and the run after `/**` begins the next component (`{x/**}~1/**` spells
    `x/CONFIG~1/`)."""
    found, free = [], _SN_KINDS - {"~"}

    def ends(confs):
        found.extend(start for s, start in confs if s in _SN_ACCEPT)

    confs = {("start", 0)}
    for k, t in enumerate(flat):
        kind = t[0]
        if kind == "lit" and t[1] != "/":
            confs = _sn_step(confs, _token_kinds(t))
        elif kind in ("any", "class"):
            kinds = _token_kinds(t)
            nxt = _sn_step(confs, kinds) | confs
            if "/" in kinds:   # the `/` that ends the component
                ends(confs)
                nxt.add(("start", k + 1))
            confs = nxt
        elif kind == "star":
            confs = _sn_closure(confs, free)
            ends(confs)
            confs = confs | _sn_closure({("start", k + 1)}, free)
        elif kind == "rprefix" and k:   # `(?:/?|.*/)`: nothing, or a run and a `/`
            ends(_sn_closure(confs, free))
            confs = confs | {("start", k + 1)}
        elif kind == "rsuffix":   # `/.*`
            ends(confs)
            confs = _sn_closure({("start", k + 1)}, free)
        else:   # a literal `/`, `/**/`, or a leading `**/`
            ends(confs)
            confs = {("start", k + 1)}
    ends(confs)
    return min(found) if found else None


def glob_wild_component(comp: list) -> bool:
    """May a name this glob component matches be one the fold key cannot
    judge, by its literal characters (rounds 24 to 26)? A literal
    component: a `wild_kind` name, or one ending in a dot or a space. One
    with wildcards, coarsely: its literal characters hold `~`, a Cn or a Cs
    code point, or end in a dot or a space. Whether a component can spell
    an 8.3 short name is `glob_short_name_start`'s, across the alternative."""
    lits = "".join(t[1] for t in comp if t[0] == "lit")
    if all(t[0] == "lit" for t in comp):
        return bool(wild_kind(lits)) or lits != lits.rstrip(". ")
    return "~" in lits or lits != lits.rstrip(". ") or any(unicodedata.category(ch) in ("Cn", "Cs") for ch in lits)


#: What Rust's `str::trim()`, which coord applies to each glob, strips: the
#: Unicode White_Space set (`char::is_whitespace`, listed from rustc 1.95).
#: Python's `str.strip()` also strips U+001C-U+001F, which Rust keeps, so a
#: glob of them would be blank here and live in coord (round 26's F3).
_RUST_WHITESPACE = ("\t\n\x0b\x0c\r \x85\xa0\u1680\u2000\u2001\u2002\u2003\u2004\u2005"
                    "\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000")


@dataclass
class CodePaths:
    source: str = "none"          # arg | coord | none
    state: str = "not_needed"     # declared | undeclared | unreadable | not_needed
    globs: list = field(default_factory=list)
    dropped: list = field(default_factory=list)
    note: str = ""
    matchers: list = field(default_factory=list)
    fold_matchers: list = field(default_factory=list)
    wild_globs: list = field(default_factory=list)

    @property
    def complete(self) -> bool:
        return bool(self.matchers) and not self.dropped

    def load(self, globs: list) -> None:
        self.globs = list(globs)
        for g in globs:
            g = g.strip(_RUST_WHITESPACE)
            if not g:
                continue  # coord's hygiene: a blank entry is not a declaration
            try:
                tokens = glob_tokens(g)
                self.matchers.append(glob_matcher(tokens))
                folded, wild = self.load_folded(g, tokens)
            except (GlobError, re.error) as exc:
                self.dropped.append("%s (%s)" % (g, exc))
                continue
            self.fold_matchers += folded
            self.wild_globs += wild
        self.state = "declared" if (self.matchers or self.dropped) else "undeclared"

    def load_folded(self, g: str, tokens: list) -> tuple:
        """(fold matchers, wild entries): the oracle's own spellings go
        through the tree's fold and wild rules (rounds 23 to 26). Each
        alternative of the glob, as globset parses it, also matches by
        `fold_key` through `folded_glob_regex`, so `Config/**`, `[!c]onfig/**`
        and `caf?/**` all hold `config/x` or an NFD `café/x`. And in each
        alternative, the first component `glob_wild_component` flags, and the
        first `glob_short_name_start` finds, each make unknown every path with
        an entry at that level in a directory the tokens before it can
        match: `CONFIG~1/**` may name `configuration/`, and
        `src/**/SETTIN~1.YAM` may name `src/settings.yaml`."""
        if tokens == [("rprefix",)]:
            return [re.compile(".*", re.DOTALL)], []
        alts = glob_alternatives(tokens)
        folded = [re.compile(folded_glob_regex(alt), re.DOTALL) for alt in alts]
        wild = []
        for alt in alts:   # the wild check runs on every alternative (round 24)
            starts = {glob_short_name_start(alt)}
            starts.add(next((start for start, comp in glob_components(alt) if glob_wild_component(comp)), None))
            for start in sorted(starts - {None}):
                wild.append((re.compile(folded_glob_regex(alt[:start]), re.DOTALL), g))
        return folded, wild

    def wild_glob(self, path: str) -> Optional[str]:
        """The glob whose wild component may be an entry, at some level, in a
        directory `path` passes through, or None. A wild entry's parent regex
        is matched against the key of each such directory with its `/`, the
        root's being empty."""
        parts = path.split("/")
        dirs = [""] + [fold_key("/".join(parts[:j])) + "/" for j in range(1, len(parts))]
        for parent_re, g in self.wild_globs:
            if any(parent_re.fullmatch(d) for d in dirs):
                return g
        return None

    def classify(self, path: str):
        """True: inside the declared code paths. False: outside, on a complete
        oracle. None: cannot say -- with the reason."""
        if self.state == "unreadable":
            return None, "code_paths_unreadable:" + self.note
        if not self.matchers:
            if self.dropped:
                return None, "code_paths_oracle_incomplete"
            return None, "code_paths_undeclared"
        raw = path.encode("utf-8", "surrogateescape")
        if any(m.fullmatch(raw) for m in self.matchers):
            return True, ""
        if any(m.fullmatch(fold_key(path)) for m in self.fold_matchers):
            return True, ""
        wild = self.wild_glob(path)
        if wild is not None:
            return None, "code_paths_wild:" + wild
        if self.dropped:
            return None, "code_paths_oracle_incomplete"
        return False, ""


# ---------------------------------------------------------------------------
# coord: HTTP and the credential
# ---------------------------------------------------------------------------

def coord_base_url() -> tuple:
    fixture = os.environ.get("EXEC_EQ_COORD_URL", "").strip()
    if fixture:
        return fixture.rstrip("/"), True
    return (os.environ.get("COORD_HTTP_URL", "").strip() or DEFAULT_COORD_URL).rstrip("/"), False


def coord_url_ok(url: str) -> bool:
    """A bearer is sent only to https, or to plain http on a LOOPBACK address."""
    return bool(re.fullmatch(r"https://[A-Za-z0-9.-]+(?::[0-9]+)?(?:/[^@\s]*)?", url)
                or re.fullmatch(r"http://(?:127\.0\.0\.1|\[::1\])(?::[0-9]+)?(?:/[^@\s]*)?", url))


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect would carry the bearer to wherever it points; refuse it."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


def http_json(method: str, url: str, bearer: str, payload=None):
    """(status, parsed-json-or-None, error-text). status None: no answer at all."""
    data = None
    headers = {"Authorization": "Bearer " + bearer, "Accept": "application/json"}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with _OPENER.open(req, timeout=HTTP_TIMEOUT_SECS) as resp:
            status, body = resp.status, resp.read()
    except urllib.error.HTTPError as exc:
        try:
            status, body = exc.code, exc.read() if exc.fp else b""
        except (OSError, http.client.HTTPException):
            status, body = exc.code, b""
    except (urllib.error.URLError, OSError, ValueError, http.client.HTTPException) as exc:
        # http.client.HTTPException (an incomplete read, a malformed status
        # line) is not an OSError: uncaught it would read as an internal error
        # rather than as the coord that did not answer.
        reason = getattr(exc, "reason", exc)
        return None, None, str(reason)[:200]
    try:
        parsed = json.loads(body.decode("utf-8")) if body else None
    except (UnicodeDecodeError, ValueError):
        parsed = None
    return status, parsed, ""


@dataclass
class Credential:
    state: str = "not_needed"     # proven | any | none | not_needed
    tenant: str = ""
    source: str = ""
    note: str = ""
    owner_state: str = ""         # proven | refuted | unknown | not_tried
    bearer: str = field(default="", repr=False)


def machine_active_tenant() -> Optional[str]:
    home = os.environ.get("HOME") or os.environ.get("USERPROFILE") or ""
    path = os.path.join(home, ".qontinui", "machine.json")
    try:
        with open(path, "r", encoding="utf-8") as fh:
            doc = json.load(fh)
    except (OSError, ValueError):
        return None
    tenant = doc.get("active_tenant_id") if isinstance(doc, dict) else None
    if isinstance(tenant, str) and re.fullmatch(r"[0-9A-Fa-f]{8}-(?:[0-9A-Fa-f]{4}-){3}[0-9A-Fa-f]{12}", tenant):
        return tenant
    return None


# jwt-cascade-selection: this file implements no credential cascade and reads
# no token source -- it names the two static sources only in its docstring.
# lib/coord-tenant-credential.sh, which stage-coord-bearer.sh sources, selects
# each source on VALIDITY (an exp more than 60 s away AND the tenant claim
# asked for), never on presence.
def stage_credential(coord_url: str, slug: Optional[str], need_owner: bool) -> Credential:
    """Stage a coord bearer through lib/stage-coord-bearer.sh. `need_owner`:
    the tenant must be PROVEN to own `slug` (code_paths are read under it);
    when it cannot be, the result still carries an `any` credential for the
    auth-only Python route, with owner_state saying why."""
    cred = Credential()
    script = os.path.join(LIB, "stage-coord-bearer.sh")
    if not os.path.isfile(script):
        cred.state, cred.note = "none", "lib/stage-coord-bearer.sh is missing"
        return cred
    bash = resolve_bash()
    if bash.path is None:
        cred.state, cred.note = "none", "no usable bash: " + bash.reason
        return cred
    candidates = []
    env_tenant = os.environ.get("QONTINUI_TENANT_ID", "").strip()
    if re.fullmatch(r"[0-9A-Fa-f]{8}-(?:[0-9A-Fa-f]{4}-){3}[0-9A-Fa-f]{12}", env_tenant):
        candidates.append(env_tenant)
    active = machine_active_tenant()
    if active and active.lower() not in (c.lower() for c in candidates):
        candidates.append(active)
    tmp = tempfile.mkdtemp(prefix="exec-eq-")
    try:
        env = dict(os.environ)
        env["CTC_TMP"] = tmp.replace("\\", "/")
        env["CTC_COORD_URL"] = coord_url
        env["CTC_PY"] = sys.executable.replace("\\", "/")
        if need_owner and slug:
            argv = [bash.path, script.replace("\\", "/"), "owner", slug, *candidates]
        else:
            argv = [bash.path, script.replace("\\", "/"), "any", *candidates]
        try:
            proc = subprocess.run(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                  timeout=STAGE_TIMEOUT_SECS)
        except (OSError, subprocess.TimeoutExpired) as exc:
            cred.state, cred.note = "none", "the credential stage did not complete: %s" % exc
            return cred
        facts = {}
        for line in proc.stdout.decode("utf-8", "replace").splitlines():
            key, sep, value = line.partition("=")
            if sep:
                facts[key.strip()] = value.strip()
        cred.owner_state = facts.get("owner_state", "proven" if facts.get("state") == "proven" else
                                     ("not_tried" if not (need_owner and slug) else "unknown"))
        cred.tenant = facts.get("tenant", "")
        cred.source = facts.get("source", "")
        cred.note = facts.get("owner_note") or facts.get("note", "")
        state = facts.get("state", "")
        if state not in ("proven", "any"):
            cred.state = "none"
            if not cred.note:
                err = proc.stderr.decode("utf-8", "replace").strip().splitlines()
                cred.note = "the stage answered no credential" + (": " + err[-1] if err else "")
            return cred
        try:
            with open(os.path.join(tmp, "bearer.hdr"), "r", encoding="ascii") as fh:
                header = fh.read().strip()
        except (OSError, UnicodeDecodeError):
            cred.state, cred.note = "none", "the stage reported a credential but staged no bearer"
            return cred
        token = header[len("Authorization: Bearer "):] if header.startswith("Authorization: Bearer ") else ""
        if not re.fullmatch(r"[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*", token):
            cred.state, cred.note = "none", "the staged bearer is not a JWT"
            return cred
        cred.state, cred.bearer = state, token
        return cred
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def read_code_paths_from_coord(coord_url: str, slug: Optional[str], cred: Credential) -> CodePaths:
    cp = CodePaths(source="coord")
    if not slug:
        cp.state, cp.note = "unreadable", "the origin is not a GitHub owner/name, so no tenant can be proven to own it"
        return cp
    if cred.state != "proven":
        cp.state = "unreadable"
        cp.note = "no credential for the tenant that owns %s (%s)" % (slug, cred.note or cred.owner_state or cred.state)
        return cp
    url = "%s%s?repo=%s" % (coord_url, SCOPE_ROUTE, urllib.parse.quote(slug, safe=""))
    status, doc, err = http_json("GET", url, cred.bearer)
    if status is None:
        cp.state, cp.note = "unreadable", "coord did not answer: " + err
        return cp
    if status != 200:
        cp.state, cp.note = "unreadable", "coord answered HTTP %d" % status
        return cp
    globs = doc.get("code_paths") if isinstance(doc, dict) else None  # the route's own field
    if not isinstance(globs, list) or not all(isinstance(g, str) for g in globs):
        cp.state, cp.note = "unreadable", "coord's answer carries no code_paths list"
        return cp
    cp.load(globs)
    return cp


# ---------------------------------------------------------------------------
# language detection
# ---------------------------------------------------------------------------

def shebang_lang(content: Optional[bytes]) -> Optional[str]:
    if not content or not content.startswith(b"#!"):
        return None
    first = content.split(b"\n", 1)[0].decode("utf-8", "replace").strip()
    words = first[2:].strip().split()
    if not words:
        return None
    interp = posixpath.basename(words[0])
    if interp == "env":
        rest = [w for w in words[1:] if not w.startswith("-") and "=" not in w]
        if not rest:
            return None
        interp = posixpath.basename(rest[0])
    interp = re.sub(r"[0-9.]+$", "", interp) if interp.startswith("python") else interp
    return SHEBANG_LANG.get(interp)


def detect_lang(change: Change) -> Optional[str]:
    ext = posixpath.splitext(change.path)[1].lower()
    if ext in EXT_LANG:
        return EXT_LANG[ext]
    if ext == "":
        return shebang_lang(change.head) or shebang_lang(change.base)
    return None


def is_migration_path(path: str) -> bool:
    """coord's `pr_merge::migration_classifier::is_migration_path`, verbatim in
    behaviour: its merge engine applies the migration proof to exactly these."""
    norm = path.replace("\\", "/").lower()
    return "alembic/versions/" in norm or norm.startswith("migrations/") or "/migrations/" in norm


# ---------------------------------------------------------------------------
# the Python arm
# ---------------------------------------------------------------------------

#: Tool directives a Python comment carries. Coord's proof treats every comment
#: as inert; these are refused CLIENT side, on top of a `true`, like `>>>`.
PY_DIRECTIVE_RES = [
    re.compile(r"#\s*type:\s*\S[^\n#]*"),
    re.compile(r"#\s*noqa\b[^\n#]*"),
    re.compile(r"#\s*(?:pragma|pylint|isort|pyright|mypy|ruff|fmt|yapf|black)\s*:\s*[^\n#]*"),
    re.compile(r"#\s*nosec\b[^\n#]*"),
]


def py_statement_strings(src: bytes) -> Optional[list]:
    """Every statement-position plain string (docstrings and bare string
    statements), in source order; None when this interpreter cannot parse it."""
    try:
        tree = ast.parse(src)
    except (SyntaxError, ValueError, MemoryError, RecursionError):
        return None
    found = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
            found.append((node.lineno, node.col_offset, node.value.value))
    found.sort()
    return [s for _, _, s in found]


def py_directives(src: bytes) -> list:
    text = src.decode("utf-8", "replace")
    frags = []
    for rx in PY_DIRECTIVE_RES:
        frags.extend(" ".join(m.group(0).split()) for m in rx.finditer(text))
    return sorted(frags)


def first_line(src: bytes) -> bytes:
    return src.split(b"\n", 1)[0].rstrip(b"\r")


def python_client_refusal(base: bytes, head: bytes) -> tuple:
    """The client-side refusals on top of coord's `true`: (verdict, reason)."""
    if (base.startswith(b"#!") or head.startswith(b"#!")) and first_line(base) != first_line(head):
        return EXECUTABLE, "shebang_changed"
    if py_directives(base) != py_directives(head):
        return EXECUTABLE, "directive_comment"
    if b">>>" in base or b">>>" in head:
        bs, hs = py_statement_strings(base), py_statement_strings(head)
        if bs is None or hs is None or len(bs) != len(hs):
            return UNKNOWN, "doctest_rule_unparseable"
        for b, h in zip(bs, hs):
            if b != h and (">>>" in b or ">>>" in h):
                return EXECUTABLE, "doctest_marker"
    return NON_EXECUTABLE, ""


def python_arm(change: Change, coord_url: str, cred: Credential) -> None:
    change.arm = "python"
    if change.status != "M" or change.base is None or change.head is None:
        return
    profile = "migration" if is_migration_path(change.path) else "general"
    if len(change.base) > SIDE_CAP_BYTES or len(change.head) > SIDE_CAP_BYTES:
        change.verdict, change.reason = UNKNOWN, "python_side_over_1MiB"
        return
    try:
        base_text, head_text = change.base.decode("utf-8"), change.head.decode("utf-8")
    except UnicodeDecodeError:
        change.verdict, change.reason = UNKNOWN, "python_side_not_utf8"
        return
    if cred.state not in ("proven", "any"):
        change.verdict, change.reason = UNKNOWN, "no_coord_credential:" + (cred.note or cred.state)
        return
    status, doc, err = http_json("POST", coord_url + PYTHON_ROUTE, cred.bearer,
                                 {"path": change.path, "base": base_text, "head": head_text, "profile": profile})
    if status is None:
        change.verdict, change.reason = UNKNOWN, "coord_unreachable:" + err
        return
    if status != 200:
        change.verdict, change.reason = UNKNOWN, "coord_http_%d" % status
        return
    proven = doc.get("non_executable") if isinstance(doc, dict) else None
    echoed = doc.get("profile") if isinstance(doc, dict) else None
    if not isinstance(proven, bool) or echoed != profile:
        change.verdict, change.reason = UNKNOWN, "coord_bad_answer"
        return
    if not proven:
        change.verdict, change.reason = EXECUTABLE, "coord_refused_proof:" + profile
        return
    verdict, why = python_client_refusal(change.base, change.head)
    if verdict != NON_EXECUTABLE:
        change.verdict, change.reason = verdict, why
        return
    change.verdict, change.reason = NON_EXECUTABLE, "coord_proof:" + profile


# ---------------------------------------------------------------------------
# the tree-sitter arms
# ---------------------------------------------------------------------------

class TreeSitter:
    def __init__(self):
        self.state = "not_needed"
        self.found = ""
        self.core = ""
        self.reason = ""
        self._pack = None
        self._parsers = {}

    def load(self) -> None:
        import importlib.metadata as md
        try:
            self.found = md.version(TREE_SITTER_PACK)
        except md.PackageNotFoundError:
            self.state, self.reason = "missing", "tree_sitter_pack_missing:%s==%s" % (TREE_SITTER_PACK, TREE_SITTER_PACK_PIN)
            return
        except Exception as exc:  # a broken install, not a verdict
            self.state, self.reason = "missing", "tree_sitter_pack_unreadable:%s" % type(exc).__name__
            return
        if self.found != TREE_SITTER_PACK_PIN:
            self.state = "version_mismatch"
            self.reason = "tree_sitter_pack_version:%s!=%s" % (self.found, TREE_SITTER_PACK_PIN)
            return
        try:
            import tree_sitter_language_pack as pack
            self.core = md.version("tree-sitter")
        except Exception as exc:
            self.state, self.reason = "import_failed", "tree_sitter_pack_import_failed:%s" % type(exc).__name__
            return
        self._pack = pack
        self.state = "ok"

    def parser(self, lang: str):
        if lang not in self._parsers:
            try:
                self._parsers[lang] = self._pack.get_parser(lang)
            except Exception:
                self._parsers[lang] = None
        return self._parsers[lang]


#: String-like nodes, compared as ONE token holding their full text: whitespace
#: inside a literal is significant, and a grammar that leaves part of a literal
#: uncovered by any child (Rust's `r#"`) cannot hide a change there.
ATOMIC = {
    "rust": {"string_literal", "raw_string_literal", "char_literal"},
    "typescript": {"string", "template_string", "regex"},
    "tsx": {"string", "template_string", "regex"},
    "javascript": {"string", "template_string", "regex"},
    "bash": {"string", "raw_string", "ansi_c_string", "translated_string", "heredoc_body"},
}
COMMENT_TYPES = {"comment", "line_comment", "block_comment"}

#: Tool directives carried by a comment. Each match becomes a token AT ITS
#: POSITION, so changing, adding, removing or moving one is executable.
JS_DIRECTIVE_RES = [
    re.compile(r"@ts-(?:ignore|expect-error|nocheck|check)\b"),
    re.compile(r"\A///\s*<[^\n]*"),
    re.compile(r"@jsx(?:ImportSource|Runtime|Frag)?\b[^\n*]*"),
    re.compile(r"\A/[/*]\s*(?:eslint|globals?|exported)\b[^\n]*"),
    re.compile(r"\beslint-(?:disable|enable)\S*[^\n*]*"),
    re.compile(r"\bwebpack[A-Z][A-Za-z]*\s*:[^\n*,]*"),
    re.compile(r"@vite-ignore\b"),
    re.compile(r"[@#]__(?:PURE|NO_SIDE_EFFECTS|INLINE|NOINLINE|KEY|MANGLE_PROP)__"),
    re.compile(r"@(?:vitest|jest)-environment(?:-options)?\b[^\n*]*"),
    re.compile(r"@(?:no)?flow\b"),
    re.compile(r"\b(?:istanbul|c8|v8)\s+ignore\b[^\n*]*"),
    re.compile(r"\bnode:coverage\b[^\n*]*"),
    re.compile(r"\b(?:prettier-ignore|biome-ignore|deno-lint-ignore|deno-fmt-ignore|oxlint-disable|tslint:)\S*[^\n*]*"),
    re.compile(r"[#@]\s*source(?:Mapping)?URL=\S*"),
    re.compile(r"@(?:deprecated|internal)\b"),
    re.compile(r"@generated\b"),
]
SH_DIRECTIVE_RES = [
    re.compile(r"\A#\s*shellcheck\s+\S[^\n]*"),
    re.compile(r"@generated\b"),
]
RS_DIRECTIVE_RES = [
    re.compile(r"@generated\b"),
]
#: JSDoc tags whose `{type}` a JavaScript file's type checker (checkJs,
#: `// @ts-check`) reads. In TypeScript files tsc ignores them.
JSDOC_TYPE_TAGS = {
    "type", "typedef", "param", "arg", "argument", "returns", "return", "template",
    "callback", "satisfies", "implements", "extends", "augments", "enum", "this",
    "overload", "import", "property", "prop", "throws", "exception", "yields",
}
JSDOC_STRUCTURAL_TAGS = {"template", "typedef", "callback", "import", "overload"}

#: Commands that read a file's text, for the shell self-read rule.
SHELL_READERS = {
    "sed", "awk", "gawk", "mawk", "nawk", "grep", "egrep", "fgrep", "rg", "head", "tail",
    "cat", "tac", "less", "more", "cut", "tr", "sort", "uniq", "wc", "nl", "perl", "ruby",
    "python", "python3", "node", "od", "xxd", "hexdump", "strings", "base64", "md5sum",
    "sha1sum", "sha256sum", "cksum", "diff", "cmp", "comm", "paste", "fold", "fmt",
    "column", "iconv", "dd", "read", "mapfile", "readarray", "cp", "install", "ln",
    "rsync", "scp", "tee", "split", "csplit", "gzip", "bzip2", "xz", "zstd", "tar",
    "zip", "openssl", "git", "jq", "php", "lua", "rev", "expand", "pr", "sha512sum",
    "b2sum", "shasum", "sum",
}

#: Commands that run another command named among their arguments
#: (`command sed ...`, `env LC_ALL=C cat ...`, `timeout 5 head ...`): a reader
#: under one of them still reads.
SHELL_WRAPPERS = {
    "command", "builtin", "env", "exec", "xargs", "sudo", "doas", "nice", "ionice",
    "timeout", "nohup", "stdbuf", "setsid", "chrt", "taskset",
}

#: The redirect operators that READ a file. tree-sitter-bash puts an optional
#: `file_descriptor` child (`3<"$SELF"`, `0<"$0"`) before the operator, so the
#: operator is looked for among all of a redirect's children, not only the first.
SHELL_READ_REDIRECTS = ("<", "<&", "<>")

#: The bytes a language itself reads as blank between tokens, where that set
#: is NARROWER than the gaps its tree-sitter grammar skips. tree-sitter-bash
#: skips CR, VT and FF as it skips a space, but bash splits words on space,
#: tab and newline only: `a\vb` is one word to bash and `a b` two. A gap that
#: holds any other byte is kept as a token. In TS/JS a gap may be JSX text
#: tree-sitter leaves in no node (whitespace-only text holding a newline),
#: and Babel keeps a VT or FF there as a text child, so only space, tab, CR
#: and LF are blank. Every other language here reads each byte
#: bytes.strip() removes as blank.
LANG_BLANKS = {"bash": b" \t\n", **{lang: b" \t\r\n" for lang in JS_FAMILY}}

#: The bytes after which a bash `#` starts a comment: a `#` starts one only at
#: the start of a word.
SHELL_WORD_BOUNDARY = b" \t\n;&|()<>"

#: Commands that run text they are handed as shell code, and shells that run a
#: `-c` argument. That text is parsed and checked like the script's own, so a
#: self-read inside it is seen. Text that is not a literal refuses the proof.
#: tree-sitter-bash nodes inside which a `#` never starts a comment
#: (arithmetic, `${ }` and its subscripts; a C-style for's header and a
#: compound assignment's `[ ]` key are judged in shell_comment_is_comment),
#: and the nodes that start a fresh command line within them, where it can
#: again.
SHELL_NO_COMMENT_CONTEXTS = {"arithmetic_expansion", "expansion", "subscript"}
SHELL_COMMENT_BARRIERS = {"command_substitution", "process_substitution", "subshell", "do_group",
                          "compound_statement"}
SHELL_EVALUATORS = {"eval", "trap", "alias"}
SHELL_INTERPRETERS = {"sh", "bash", "dash", "zsh", "ksh", "mksh", "ash", "busybox"}

#: What a bash script can reach its own path or text through, besides `$0`:
#: `$_` (the script's path at startup), BASH_SOURCE, BASH_ARGV0, BASH_ARGV
#: (under extdebug), the `caller` builtin, a substring of `$@` / `$*`
#: (`${@:0:1}` is `$0`), an indirect `${!x}`, the descriptor bash keeps the
#: script open on (`/proc/self/fd/255`, `/dev/fd/255`), and the file's own
#: name spelled as a literal.
SHELL_SELF_SPECIALS = {b"0", b"_"}
SHELL_SELF_VARIABLES = {b"BASH_SOURCE", b"BASH_ARGV0", b"BASH_ARGV"}
SHELL_SELF_PATHS = (b"/proc/", b"/dev/fd/")

#: JS/TS nodes whose exact source text a program can read:
#: Function.prototype.toString returns a function's or class's text, comments
#: and whitespace included, and any string conversion calls it (`"" + f`,
#: `${f}`, String(f)), so no spelling can rule the read out.
JS_SOURCE_TEXT_NODES = {
    "function_declaration", "function_expression", "function", "arrow_function",
    "method_definition", "generator_function", "generator_function_declaration",
    "class_declaration", "class", "abstract_class_declaration",
}

#: What a JS/TS module can reach its own path or text through. A CommonJS
#: module's body runs inside a wrapper function, so whatever reaches that
#: function (`arguments.callee`, a top-level function's `.caller`, the
#: module-level `arguments`) reads the module's whole text by toString(), and
#: the module registries (`require.cache`, `module.constructor`,
#: `process.mainModule`) hold its path.
JS_SELF_IDENTIFIERS = {b"__filename", b"__dirname"}
JS_SELF_MEMBERS = {
    (b"process", b"argv"), (b"process", b"mainModule"), (b"require", b"main"), (b"require", b"cache"),
    (b"module", b"filename"), (b"module", b"id"), (b"module", b"path"), (b"module", b"constructor"),
    (b"document", b"currentScript"),
}
JS_SELF_PROPERTIES = {b"stack", b"currentScript", b"callee", b"caller"}
#: The JS/TS functions that bind their own `arguments` (an arrow does not), so
#: an `arguments` outside all of them is the CommonJS wrapper's.
JS_ARGUMENTS_SCOPES = {
    "function_declaration", "function_expression", "function", "method_definition",
    "generator_function", "generator_function_declaration",
}

#: The extensions a TS/JS source is emitted or resolved under, so a literal
#: naming `x.js` names `x.ts` too.
JS_EMIT_EXTS = (".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".mts", ".cts")

#: What a Rust file can reach its own path through: `file!()`,
#: `Location::caller().file()` and CARGO_MANIFEST_DIR.
RUST_SELF_MACROS = {b"file"}
RUST_SELF_NAMES = (b"CARGO_MANIFEST_DIR", b"Location")

LINE_TERMINATORS = (b"\n", b"\r", "\u2028".encode("utf-8"), "\u2029".encode("utf-8"))


def brace_end(text: str) -> int:
    """The index just past the `}` that balances `text`'s leading `{`
    (len(text) when it never balances: the rest of the comment is the type)."""
    depth = 0
    for k, ch in enumerate(text):
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return k + 1
    return len(text)


def jsdoc_type_fragments(text: str) -> list:
    """`@tag {type} name` fragments of a JSDoc block (a description is prose)."""
    frags = []
    for m in re.finditer(r"@([A-Za-z]+)\b", text):
        tag = m.group(1)
        if tag not in JSDOC_TYPE_TAGS:
            continue
        rest = text[m.end():]
        stripped = rest.lstrip(" \t")
        if stripped.startswith("{"):
            end = brace_end(stripped)
            braces = stripped[:end]
            name = re.match(r"\s*(\[[^\]]*\]|[^\s*]+)?", stripped[end:])
            frag = "@%s %s %s" % (tag, braces, (name.group(1) or "") if name else "")
        elif tag in JSDOC_STRUCTURAL_TAGS:
            frag = "@%s %s" % (tag, rest.split("\n", 1)[0])
        else:
            name = re.match(r"\s*([^\s*]+)?", rest)
            frag = "@%s %s" % (tag, (name.group(1) or "") if name else "")
        frags.append(" ".join(frag.replace("*", " ").split()))
    return frags


def comment_directives(lang: str, text: str, at_file_start: bool) -> list:
    frags = []
    if at_file_start and text.startswith("#!"):
        frags.append("shebang " + text.split("\n", 1)[0].rstrip("\r"))
    res = JS_DIRECTIVE_RES if lang in JS_FAMILY else SH_DIRECTIVE_RES if lang == "bash" else RS_DIRECTIVE_RES
    for rx in res:
        frags.extend(" ".join(m.group(0).split()) for m in rx.finditer(text))
    if lang == "javascript" and text.startswith("/**"):
        frags.extend(jsdoc_type_fragments(text))
    return frags


def rust_doc_kind(node) -> Optional[str]:
    for child in node.children:
        if child.type == "outer_doc_comment_marker":
            return "outer"
        if child.type == "inner_doc_comment_marker":
            return "inner"
    return None


def rust_doc_text(node, src: bytes) -> str:
    """The doc comment's text after its marker, verbatim (a `/** */` doc is
    never proven inert, so no star strip is modelled)."""
    for child in node.children:
        if child.type == "doc_comment":
            text = src[child.start_byte:child.end_byte].decode("utf-8", "replace")
            return text if text.endswith("\n") else text + "\n"
    return "\n"


# A BACKTICK fence's info string may not contain a backtick (CommonMark 4.5;
# pulldown-cmark, which rustdoc uses, agrees): "```rust`x" is a paragraph line,
# not an opener. Reading it as one would shift every later open and close, so
# the backtick arm refuses a backtick in what follows the run. A tilde fence's
# info string may hold anything.
FENCE_OPEN_RE = re.compile(r"^ {0,3}(?:(`{3,})([^`]*)|(~{3,})(.*))$")


#: Rust's `char::is_whitespace` (the Unicode White_Space property). rustdoc's
#: unindent skips a line made only of these when it computes the common
#: indentation, and keeps such a line verbatim.
RUST_WHITESPACE = frozenset([chr(c) for c in (0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x20, 0x85, 0xa0, 0x1680)]
                            + [chr(c) for c in range(0x2000, 0x200b)]
                            + [chr(c) for c in (0x2028, 0x2029, 0x202f, 0x205f, 0x3000)])

#: A lone CR: CommonMark calls it a line ending, Rust's `str::lines` does not,
#: and rustdoc's measured doctests match neither model cleanly. Where WHICH
#: lines exist is in doubt, doc_layout_ambiguous refuses the proof.
LONE_CR_RE = re.compile(r"\r(?!\n)")


def md_blank(ln: str) -> bool:
    """A CommonMark blank line: nothing but spaces and tabs. Python's
    `str.strip()` would also eat NBSP, U+2000-range spaces and \\x1c-\\x1f,
    each of which makes a line NON-blank to CommonMark (and to rustdoc)."""
    return not ln.strip(" \t")


def rust_blank(ln: str) -> bool:
    """A line rustdoc's unindent ignores: every char is Rust whitespace."""
    return all(c in RUST_WHITESPACE for c in ln)


def unindented_lines(text: str) -> list:
    """The lines of a doc text after rustdoc's unindent (split at LF and CRLF;
    a lone CR is refused by doc_layout_ambiguous). The common indentation is
    counted in spaces over the lines rustdoc does not skip; a line rustdoc
    skips is kept verbatim. (A tab in a line's lead is block context:
    doc_layout_ambiguous refuses it.)"""
    lines = text.replace("\r\n", "\n").split("\n")
    widths = [len(ln) - len(ln.lstrip(" ")) for ln in lines if not rust_blank(ln)]
    cut = min(widths) if widths else 0
    return [ln if rust_blank(ln) else ln[cut:] if len(ln) - len(ln.lstrip(" ")) >= cut else ln.lstrip(" ")
            for ln in lines]


def code_blocks(text: str) -> list:
    """The code a Markdown text holds -- fenced blocks (with their info string)
    and indented blocks -- after removing the common indentation the way rustdoc
    unindents a doc comment. Prose is dropped; everything else is kept."""
    lines = unindented_lines(text)
    out = []
    fence = None
    for ln in lines:
        if fence is not None:
            # CommonMark 4.5: a closer may be followed by spaces and tabs ONLY.
            # `\s` would also accept NBSP or \x1c, closing the fence here while
            # rustdoc keeps the line, and every line after it, in the doctest.
            m = re.match(r"^ {0,3}(`{3,}|~{3,})[ \t]*$", ln)
            if m and m.group(1)[0] == fence[0] and len(m.group(1)) >= fence[1]:
                out.append(("close",))
                fence = None
            else:
                # Verbatim: trailing whitespace inside a multi-line string
                # literal is part of the doctest.
                out.append(("code", ln))
            continue
        m = FENCE_OPEN_RE.match(ln)
        if m:
            run = m.group(1) or m.group(3)
            info = m.group(2) if m.group(1) else m.group(4)
            fence = (run[0], len(run))
            # The opener's own indentation is stripped from every content line,
            # so it is part of what the block holds.
            # CommonMark trims an info string of spaces and tabs only; an NBSP
            # left in it changes the tag rustdoc reads.
            out.append(("open", info.strip(" \t"), len(ln) - len(ln.lstrip(" "))))
            continue
        if not md_blank(ln) and (ln.startswith("    ") or ln.startswith("\t")):
            out.append(("indented", ln))
    if fence is not None:
        out.append(("unclosed",))
    return out


LIST_MARKER_RE = re.compile(r"^ {0,3}([-+*]|[0-9]{1,9}[.)])( |\t|$)")


def doc_layout_ambiguous(text: str) -> bool:
    """True when WHICH lines of a Markdown text are code depends on block
    context code_blocks() does not model, so two texts whose code_blocks()
    agree can still hold different doctests.

    code_blocks() reads every 4+-space line as an indented code block, but in
    CommonMark one cannot interrupt a paragraph: inserting or removing the blank
    line before it turns prose into a doctest or back, with every line of the
    block unchanged. List items (whose content indent shifts what counts as
    indented, and whose end closes a fence they hold), block quotes (which
    hold code code_blocks() never sees) and HTML blocks (which swallow a fence
    until their end condition) are the same class. Conservative by
    design: any such line makes a changed text not provably inert."""
    if LONE_CR_RE.search(text):
        return True
    has_fence = False
    has_list = False
    for ln in unindented_lines(text):
        if md_blank(ln):
            continue
        lead = ln[:len(ln) - len(ln.lstrip(" \t"))]
        if "\t" in lead or len(lead) >= 4:
            return True
        if ln.lstrip(" ").startswith(">"):
            return True
        # A CommonMark HTML block (`<div>`, `<!--`, ...) swallows the lines
        # after it up to its end condition -- a blank line, or `-->` for a
        # comment -- so a fence inside one is raw HTML, not a doctest, and the
        # blank line that ends the block moves code in or out of the doctest
        # set with every code line unchanged. code_blocks() does not model it.
        if ln.lstrip(" ").startswith("<"):
            return True
        # A backtick run whose remainder holds a backtick is NOT a fence opener
        # (code_blocks() models that), but it is exactly the line a reader of
        # the two sides is most likely to misjudge, so it is refused as well:
        # belt and braces for the opener rule.
        bt = re.match(r"(`{3,})(.*)$", ln.lstrip(" "))
        if bt and "`" in bt.group(2):
            return True
        # A footnote definition (`[^x]: ...`) is a container block: a fence on
        # its continuation lines belongs to it, which code_blocks() does not
        # model.
        if ln.lstrip(" ").startswith("[^"):
            return True
        if "```" in ln or "~~~" in ln:
            has_fence = True
        if LIST_MARKER_RE.match(ln):
            has_list = True
    return has_fence and has_list


#: The attribute node a doc fragment of each kind may sit beside without
#: leaving its item's doc: rustdoc JOINS every doc fragment of an item, so
#: `/// a` `#[inline]` `/// b` is ONE doc, and a fence may span the attribute.
DOC_ATTR_NODES = {"outer": "attribute_item", "inner": "inner_attribute_item"}


def doc_continues(tokens: list, start: int, node_type: str) -> bool:
    """True when tokens[start:] is a run (possibly empty) of complete
    `node_type` subtrees and directive-comment tokens: nothing since the last
    doc token that ends an item's doc. Over-merging is safe (the whitelist
    then refuses the group); under-merging is not, so a directive comment
    (`// @generated`) between two fragments does not split them."""
    i, n = start, len(tokens)
    while i < n:
        if tokens[i][0] == "DIR":
            i += 1
            continue
        if tokens[i] != ("(", node_type):
            return False
        depth = 0
        while i < n:
            if tokens[i][0] == "(":
                depth += 1
            elif tokens[i][0] == ")":
                depth -= 1
            i += 1
            if depth == 0:
                break
        if depth:
            return False
    return True


# ---- the Rust doc whitelist ---------------------------------------------------
#
# A changed Rust doc group is non_executable ONLY when every clause below holds
# on BOTH sides; anything else is executable with a named reason. The clauses
# are a positive whitelist of the one shape whose rustdoc text is provably
# prose, so a construct nobody thought of fails closed instead of slipping by.

#: (iii) Characters a proven-inert doc line may not hold: every C0 control but
#: TAB (a leading tab is refused by (ii)), DEL, C1, and the Unicode line and
#: paragraph separators. A lone CR is a compile error in a doc comment.
_DOC_CONTROL_RE = re.compile("[\\x00-\\x08\\x0a-\\x1f\\x7f-\\x9f\\u2028\\u2029]")

#: (ii) A doc line whose first characters can open a Markdown block other than
#: a paragraph: a fence, HTML, a footnote definition, a block quote, a list
#: item or a table row, after at most three spaces.
_DOC_BLOCK_START_RE = re.compile(r"^ {0,3}(?:```|~~~|<|\[\^|>|\||[-+*](?:[ \t]|$)|[0-9]{1,9}[.)](?:[ \t]|$))")

#: (iv) Ancestors that put a doc comment inside a macro: its text is a token
#: the macro may stringify, re-emit as `#[doc]`, or join with other fragments.
_RS_MACRO_NODES = {"macro_invocation", "macro_definition", "macro_rule", "token_tree", "token_tree_pattern",
                   "token_repetition", "token_repetition_pattern"}

#: (iv) The item kinds an OUTER doc may document and still be proven inert. A
#: `mod` item is not one (the module's inner docs join its outer docs, possibly
#: from another file), nor a macro invocation, nor anything unlisted.
RUST_DOC_ITEMS = {
    "function_item", "function_signature_item", "struct_item", "enum_item", "union_item", "trait_item",
    "impl_item", "const_item", "static_item", "type_item", "field_declaration", "enum_variant",
    "associated_type", "macro_definition", "use_declaration", "extern_crate_declaration",
    "foreign_mod_item", "let_declaration",
}

#: (vi) Attributes known not to read doc text. Any other attribute on the
#: documented item or an enclosing item may be a proc macro that does --
#: clap's and structopt's derives turn field docs into --help text,
#: displaydoc's into a Display impl, schemars' into a schema -- so it refuses
#: the proof. Lint-tool namespaces (`clippy::`, `rustfmt::`, ...) are inert.
RUST_INERT_ATTRS = {
    "allow", "warn", "deny", "forbid", "expect", "deprecated", "must_use", "inline", "cold", "track_caller",
    "cfg", "test", "ignore", "should_panic", "bench", "derive", "repr", "non_exhaustive", "doc",
    "macro_export", "macro_use", "no_mangle", "export_name", "link_section", "link_name", "used",
    "automatically_derived", "default", "serde", "global_allocator", "panic_handler", "target_feature",
    "no_link", "path",
}
RUST_INERT_ATTR_NAMESPACES = ("clippy", "rustfmt", "rustdoc", "diagnostic")
#: (vi) Derives known not to read doc text, by their full path as written.
RUST_INERT_DERIVES = {
    "Debug", "Clone", "Copy", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash", "Default",
    "Serialize", "Deserialize", "serde::Serialize", "serde::Deserialize",
}
for _std in ("std", "core", "::std", "::core"):
    for _mod, _names in (("fmt", ("Debug",)), ("clone", ("Clone",)), ("marker", ("Copy",)),
                         ("cmp", ("PartialEq", "Eq", "PartialOrd", "Ord")), ("hash", ("Hash",)),
                         ("default", ("Default",))):
        RUST_INERT_DERIVES.update("%s::%s::%s" % (_std, _mod, _n) for _n in _names)


def _node_text(node, src: bytes) -> str:
    return src[node.start_byte:node.end_byte].decode("utf-8", "replace")


def _doc_raw_line(node, src: bytes) -> str:
    """The text of one `///` / `//!` fragment after removing ONLY its marker:
    no unindent (cut=0), the line ending dropped."""
    for child in node.children:
        if child.type == "doc_comment":
            text = _node_text(child, src)
            if text.endswith("\r\n"):
                return text[:-2]
            return text[:-1] if text.endswith("\n") else text
    return ""


def _doc_line_refusal(line: str) -> Optional[str]:
    """Clauses (ii) and (iii) for one raw doc line."""
    if "\r" in line:
        return "rust_doc_bare_cr"
    if _DOC_CONTROL_RE.search(line):
        return "rust_doc_control_char"
    lead = line[:len(line) - len(line.lstrip(" \t"))]
    if "\t" in lead:
        return "rust_doc_line_tab_indent"
    if len(lead) >= 4:
        return "rust_doc_line_indented"
    if _DOC_BLOCK_START_RE.match(line):
        return "rust_doc_line_block_start"
    return None


def rust_attribute_reader(attr_item, src: bytes) -> Optional[str]:
    """None when an attribute is known not to read doc text, else its name
    (clause vi). `derive` is inert only when every derived path is."""
    attr = next((c for c in attr_item.children if c.type == "attribute"), None)
    if attr is None or not attr.children:
        return "attribute_unparsed"
    name = "".join(_node_text(attr.children[0], src).split())
    if name.split("::")[0] in RUST_INERT_ATTR_NAMESPACES:
        return None
    if name not in RUST_INERT_ATTRS:
        return name
    if name != "derive":
        return None
    args = next((c for c in attr.children if c.type == "token_tree"), None)
    if args is None:
        return "derive"
    for path in _node_text(args, src)[1:-1].split(","):
        path = "".join(path.split())
        if path and path not in RUST_INERT_DERIVES:
            return "derive(%s)" % path
    return None


def _is_attr_or_comment(node) -> bool:
    return node.type in COMMENT_TYPES or node.type in ("attribute_item", "inner_attribute_item")


def _sibling_run(siblings: list, lo: int, hi: int) -> tuple:
    """The attributes and comments around siblings[lo..hi], and the first
    other node after them (the documented item, or None)."""
    run = []
    k = lo - 1
    while k >= 0 and _is_attr_or_comment(siblings[k]):
        run.append(siblings[k])
        k -= 1
    k = hi + 1
    while k < len(siblings) and _is_attr_or_comment(siblings[k]):
        run.append(siblings[k])
        k += 1
    return run, (siblings[k] if k < len(siblings) else None)


def _is_doc_fragment_attr(attr_item, src: bytes) -> bool:
    """`doc = ...` anywhere in an attribute (`#[doc = ".."]`,
    `#[doc = include_str!(..)]`, `#[cfg_attr(x, doc = ..)]`): a doc FRAGMENT
    rustdoc joins with the group. `#[doc(hidden)]` is not one."""
    return bool(re.search(r"\bdoc\s*=(?!=)", _node_text(attr_item, src)))


def _sibling_index(siblings: list, node) -> Optional[int]:
    return next((k for k, s in enumerate(siblings)
                 if s.start_byte == node.start_byte and s.end_byte == node.end_byte and s.type == node.type), None)


def rust_doc_item_refusal(first, item) -> Optional[str]:
    """Clause (iv): the doc is not inside a macro (its text is then a token
    the macro may stringify, re-emit as `#[doc]` or join with call-site
    docs), and an outer doc documents an item listed in RUST_DOC_ITEMS --
    never a `mod` item, whose inner docs join its outer docs, possibly from
    another file. `item` is None for a file's own inner docs."""
    p = first.parent
    while p is not None:
        if p.type in _RS_MACRO_NODES:
            return "rust_doc_in_macro"
        p = p.parent
    if item is None:
        return None
    if item.type == "mod_item":
        return "rust_doc_documents_mod"
    if item.type not in RUST_DOC_ITEMS:
        return "rust_doc_item:" + item.type
    return None


_DOC_WORD_RE = re.compile(r"\bdoc\b")


def _subtree_has_doc_fragment(item, src: bytes) -> bool:
    """Clause (vii): True when the item's subtree holds an inner doc comment
    (`//!` / `/*!`) or any attribute naming `doc` (`#![doc = ..]`,
    `#[cfg_attr(x, doc = ..)]`, even `#[doc(hidden)]`). Blunt by design."""
    stack = list(item.children)
    while stack:
        n = stack.pop()
        if n.type in COMMENT_TYPES:
            text = src[n.start_byte:n.end_byte]
            if rust_doc_kind(n) == "inner" or text.startswith(b"//!") or text.startswith(b"/*!"):
                return True
            continue
        if n.type in ("attribute_item", "inner_attribute_item") and _DOC_WORD_RE.search(_node_text(n, src)):
            return True
        stack.extend(n.children)
    return False


def rust_doc_group_refusal(nodes: list, kind: str, src: bytes) -> Optional[str]:
    """Clauses (i)-(vii) for one side of one doc group: None when the
    group is proven to be prose, else the named reason it is not. (v)'s
    parent-module half needs the repository and is rust_doc_whitelist()'s."""
    # The block-doc refusal: a `/** */` or `/*! */` fragment anywhere in the
    # group (rustdoc's star strip and its unindent across fragment kinds are
    # not modelled).
    if any(n.type != "line_comment" for n in nodes):
        return "rust_doc_block_comment"
    # (i) consecutive `///` (or `//!`) lines, nothing between them.
    for a, b in zip(nodes, nodes[1:]):
        if not src[a.start_byte:a.end_byte].endswith(b"\n") or src[a.end_byte:b.start_byte].strip(b" \t"):
            return "rust_doc_not_contiguous"
    # (ii) + (iii), each raw line measured at cut=0.
    for n in nodes:
        why = _doc_line_refusal(_doc_raw_line(n, src))
        if why:
            return why
    parent = nodes[0].parent
    siblings = list(parent.children) if parent is not None else []
    lo, hi = _sibling_index(siblings, nodes[0]), _sibling_index(siblings, nodes[-1])
    if lo is None or hi is None:
        return "rust_doc_unparented"
    run, item = _sibling_run(siblings, lo, hi)
    # Another fragment of the SAME item beside the group: a same-kind doc
    # comment (the grouping should have merged it -- a belt), or a
    # `doc = ...` attribute of the same kind, which rustdoc joins. The other
    # kind belongs to another item (a module's `//!` beside an item's `///`).
    attr_type = DOC_ATTR_NODES[kind]
    own_attrs = [n for n in run if n.type == attr_type]
    if kind == "outer" and item is not None:
        own_attrs += [c for c in item.children if c.type == "attribute_item"]
    for n in run:
        if n.type in COMMENT_TYPES and rust_doc_kind(n) == kind:
            return "rust_doc_adjacent_fragment"
    for n in own_attrs:
        if _is_doc_fragment_attr(n, src):
            return "rust_doc_joined_attribute"
    if kind == "inner":
        # (iv) not inside a macro; (v) inner docs document the module they
        # sit in, and only the file's own are proven inert.
        why = rust_doc_item_refusal(nodes[0], None)
        if why:
            return why
        return None if parent.type == "source_file" else "rust_doc_inner_in_block"
    why = rust_doc_item_refusal(nodes[0], item)
    if why:
        return why
    if item is None:
        return "rust_doc_no_item"
    # (vii) rustdoc JOINS an item's outer docs with the inner docs at the top
    # of its body (`fn f() { //! .. }`, `impl S { #![doc = ".."] }`) and
    # unindents them together, so a `///` edit can move a body line into or
    # out of a doctest. Blunt: any inner doc comment or any `doc` attribute
    # anywhere in the item's subtree refuses the proof.
    if _subtree_has_doc_fragment(item, src):
        return "rust_doc_joined_body_inner"
    # (vi) no attribute that may read the doc, on the item or on any item
    # enclosing it. An item's outer attributes are its preceding siblings (a
    # list node's own attribute children belong to its OTHER members).
    attrs = list(own_attrs)
    a = item.parent
    while a is not None and a.parent is not None:
        sibs = list(a.parent.children)
        k = _sibling_index(sibs, a)
        if k is not None:
            arun, _ = _sibling_run(sibs, k, k)
            attrs += [s for s in arun if s.type == "attribute_item" and s.end_byte <= a.start_byte]
        a = a.parent
    for n in attrs:
        name = rust_attribute_reader(n, src)
        if name:
            return "rust_doc_attribute_reader:" + name
    return None


@dataclass
class Reduced:
    tokens: list
    lines: list            # the 1-based line each token starts on
    docs: list             # Rust doc groups: [text, ...] in order
    has_error: bool
    #: per doc group, parallel to `docs`
    doc_kinds: list = field(default_factory=list)        # outer | inner
    doc_nodes: list = field(default_factory=list)        # the group's comment nodes, in order


def reduce_tree(lang: str, tree, src: bytes) -> Reduced:
    """The executable reduction of one side: structure, leaves and atomic
    literals, directive and doc markers; comment prose removed."""
    atomic = ATOMIC[lang]
    tokens, lines, docs = [], [], []
    doc_kinds, doc_nodes = [], []
    last_doc = None        # (kind, index in tokens) of the latest DOC token
    last_leaf_end = None
    js = lang in JS_FAMILY
    blanks = LANG_BLANKS.get(lang)

    def line_of(byte):
        return src.count(b"\n", 0, byte) + 1

    def push(tok, byte):
        tokens.append(tok)
        lines.append(line_of(byte))

    def leaf(tok, start, end):
        nonlocal last_leaf_end
        if js and last_leaf_end is not None:
            gap = src[last_leaf_end:start]
            if any(t in gap for t in LINE_TERMINATORS):
                push(("NL",), start)
        push(tok, start)
        last_leaf_end = end

    root = tree.root_node
    stack = [("enter", root)]
    while stack:
        item = stack.pop()
        kind = item[0]
        if kind == "exit":
            tokens.append((")",))
            lines.append(lines[-1] if lines else 1)
            continue
        if kind == "gap":
            _, start, end = item
            seg = src[start:end]
            if seg.strip(blanks):
                leaf(("U", seg), start, end)
            continue
        node = item[1]
        if node.type in COMMENT_TYPES:
            text = src[node.start_byte:node.end_byte]
            if lang == "bash" and not shell_comment_is_comment(node, src):
                leaf(("L", node.type, text), node.start_byte, node.end_byte)
                continue
            doc = rust_doc_kind(node) if lang == "rust" else None
            if doc:
                if last_doc is not None and last_doc[0] == doc and doc_continues(tokens, last_doc[1] + 1, DOC_ATTR_NODES[doc]):
                    # The same item's doc, continued (past attributes and
                    # directive comments, if any): one group, as rustdoc joins
                    # it. A fragment after one still marks its PRESENCE in the
                    # token stream.
                    if last_doc[1] != len(tokens) - 1:
                        push(("DOC", doc), node.start_byte)
                        last_doc = (doc, len(tokens) - 1)
                    docs[-1] += rust_doc_text(node, src)
                    doc_nodes[-1].append(node)
                else:
                    push(("DOC", doc), node.start_byte)
                    last_doc = (doc, len(tokens) - 1)
                    docs.append(rust_doc_text(node, src))
                    doc_kinds.append(doc)
                    doc_nodes.append([node])
                continue
            for frag in comment_directives(lang, text.decode("utf-8", "replace"), node.start_byte == 0):
                push(("DIR", frag), node.start_byte)
            continue
        if node.child_count == 0 or node.type in atomic:
            leaf(("L", node.type, src[node.start_byte:node.end_byte]), node.start_byte, node.end_byte)
            continue
        push(("(", node.type), node.start_byte)
        stack.append(("exit", node))
        items = []
        prev = node.start_byte
        for child in node.children:
            if child.start_byte < prev:
                raise ValueError("overlapping children in the parse tree")
            items.append(("gap", prev, child.start_byte))
            items.append(("enter", child))
            prev = child.end_byte
        items.append(("gap", prev, node.end_byte))
        stack.extend(reversed(items))
    return Reduced(tokens, lines, docs, root.has_error, doc_kinds, doc_nodes)


def shell_reading_redirect(node) -> bool:
    """A file_redirect that reads: `<`, `<&` or `<>`, numbered or not."""
    return node.type == "file_redirect" and any(ch.type in SHELL_READ_REDIRECTS for ch in node.children)


def shell_command_reads(node, src: bytes) -> bool:
    """A command node that runs a text reader, directly or under a wrapper.

    Under a wrapper, EVERY non-option word after it is checked, not only the
    first: `timeout 5 cat`, `sudo -u x cat` and `nice -n 5 cat` put an option's
    argument first, and telling an option's argument from the wrapped command
    would need each wrapper's option grammar. Conservative by design.

    A command whose name is computed (`$c "$0"` with `c=cat`), or a wrapper
    handed a computed word, may run any reader, so it counts as one."""
    words = shell_command_words(node)
    if not words:
        return False
    name = shell_literal(words[0], src)
    if name is None:  # a computed command name
        return True
    base = posixpath.basename(name.decode("utf-8", "replace"))
    if base in SHELL_READERS:
        return True
    if base in SHELL_WRAPPERS:
        for child in words[1:]:
            lit = shell_literal(child, src)
            if lit is None:  # a computed word under a wrapper
                return True
            word = lit.decode("utf-8", "replace")
            if not word or word.startswith("-") or "=" in word:
                continue
            if posixpath.basename(word) in SHELL_READERS:
                return True
    return False


#: Children of a tree-sitter-bash `command` that are not its words.
SHELL_NON_WORDS = {"file_redirect", "heredoc_redirect", "herestring_redirect", "heredoc_body", "comment",
                   "variable_assignment"}


def shell_command_words(node) -> list:
    """A `command` node's name and every argument word after it, whatever
    node type tree-sitter gives the word (`$'..'`, `$"..."`, `$((..))` and
    any other included) -- redirects and prefix assignments aside. Empty
    when the command has no name."""
    kids = [ch for ch in node.children if ch.is_named]
    at = next((i for i, ch in enumerate(kids) if ch.type == "command_name"), None)
    if at is None:
        return []
    return [kids[at]] + [ch for ch in kids[at + 1:] if ch.type not in SHELL_NON_WORDS]


def shell_comment_is_comment(node, src: bytes) -> bool:
    """Whether bash reads a tree-sitter `comment` node as a comment. A `#`
    starts one only at the start of a word: at the first byte, or after an
    unescaped space, tab, newline or metacharacter (SHELL_WORD_BOUNDARY) --
    never after CR, VT or FF, which tree-sitter skips and bash keeps in the
    word, nor after a `\\`+newline continuation. Inside `$(( ))`, `(( ))`,
    `$[ ]` and a C-style `for (( ))` header (before its `))`, not after) the
    text reaches the arithmetic evaluator, and inside `${ }` (its offsets and
    subscripts included), an assignment's `[ ]` subscript and a compound
    assignment's `[ ]` key (shell_array_key_open) bash reads it as part of
    the word; only a
    command substitution, subshell or body nested there starts a fresh
    command line (SHELL_COMMENT_BARRIERS). Anywhere else tree-sitter's
    comment is not one bash reads, and it is kept as code. (A
    comment inside backquotes that runs past the closing backquote leaves
    tree-sitter a MISSING node, so the file is a parse error, never proven.)"""
    start = node.start_byte
    if start > 0:
        if src[start - 1:start] not in SHELL_WORD_BOUNDARY:
            return False
        k, slashes = start - 1, 0
        while k > 0 and src[k - 1:k] == b"\\":
            slashes += 1
            k -= 1
        if slashes % 2:
            return False
    if node.parent is not None and node.parent.type == "array" and shell_array_key_open(node, src):
        return False
    parent = node.parent
    while parent is not None and parent.type not in SHELL_COMMENT_BARRIERS:
        if parent.type in SHELL_NO_COMMENT_CONTEXTS:
            return False
        if parent.type == "c_style_for_statement":
            close = next((ch for ch in parent.children if ch.type == "))"), None)
            if close is None or node.start_byte < close.start_byte:
                return False
        parent = parent.parent
    return True


def shell_array_key_open(node, src: bytes) -> bool:
    """Whether a comment inside a compound assignment's `( )` lies within a
    `[ ]` key bash reads whole (`([k #x<LF>]=v)`): tree-sitter-bash splits
    such a key into words around the comment, so the `[` before it is still
    open. Brackets are counted over the array's earlier words, only where
    bash reads them unquoted (shell_unquoted_bytes); an unbalanced count reads
    as open, which keeps the comment as code."""
    depth = 0
    for ch in node.parent.children:
        if ch.start_byte >= node.start_byte:
            break
        if ch.type in COMMENT_TYPES:
            continue
        text = shell_unquoted_bytes(ch, src)
        depth = max(0, depth + text.count(b"[") - text.count(b"]"))
    return depth > 0


def shell_unquoted_bytes(node, src: bytes) -> bytes:
    """The bytes of a word bash reads unquoted: every leaf outside quoting
    and expansions -- a `word`, and the pieces tree-sitter gives a
    `brace_expression` (`{`, `3`, `..`, `}`) or any other unquoted
    construct -- with each backslash-escaped byte dropped. Quoted strings,
    `$'..'` and expansions contribute nothing."""
    if node.type in ("string", "raw_string", "ansi_c_string", "translated_string") or node.type in SHELL_EXPANSIONS:
        return b""
    if node.type == "word" or node.child_count == 0:
        return re.sub(rb"\\.", b"", src[node.start_byte:node.end_byte], flags=re.S)
    return b"".join(shell_unquoted_bytes(ch, src) for ch in node.children)


def shell_self_refs(tree, src: bytes, own: bytes = b"") -> list:
    """Every node through which the script may name its own path or text
    (SHELL_SELF_SPECIALS, SHELL_SELF_VARIABLES, `${@:..}` / `${*:..}`, an
    indirect `${!x}`, `caller`, a SHELL_SELF_PATHS literal, or a literal holding
    the file's own name `own`). Expansions inside single quotes are not
    expansions; a literal path in them still names the file."""
    refs, stack = [], [tree.root_node]
    while stack:
        node = stack.pop()
        kind = node.type
        if kind in COMMENT_TYPES:
            continue
        text = src[node.start_byte:node.end_byte]
        if kind in ("word", "raw_string", "string_content") and (
                any(p in text for p in SHELL_SELF_PATHS) or (own and own in text)):
            refs.append(node)
        if kind == "raw_string":
            continue
        if kind == "special_variable_name" and text in SHELL_SELF_SPECIALS:
            refs.append(node)
        elif kind == "variable_name" and text in SHELL_SELF_VARIABLES:
            refs.append(node)
        elif kind == "expansion":
            kids = [ch.type for ch in node.children]
            if len(kids) > 1 and kids[1] == "!":
                refs.append(node)
            elif (len(kids) > 2 and kids[1] == "special_variable_name"
                  and src[node.children[1].start_byte:node.children[1].end_byte] in (b"@", b"*")
                  and ":" in kids):
                refs.append(node)
        elif kind == "command_name" and posixpath.basename(text.strip(b"'\"")) == b"caller":
            refs.append(node)
        stack.extend(node.children)
    return refs


def shell_self_reads(tree, src: bytes, own: bytes = b"") -> bool:
    """True when the script reads its OWN text: a shell_self_refs() node (an
    expansion outside single quotes, or a literal path) handed to a text
    reader, or redirected into a statement with `<` / `<&` / `<>` (numbered or
    not). A reader run under a wrapper (`command`, `env`, `timeout`, ...) is a
    reader."""
    for ref in shell_self_refs(tree, src, own):
        node = ref.parent
        while node is not None:
            if shell_reading_redirect(node):
                return True
            if node.type == "command":
                if shell_command_reads(node, src):
                    return True
                break
            node = node.parent
    return False


def shell_literal(node, src: bytes) -> Optional[bytes]:
    """A word's value after quote removal -- `$'..'` decoded as bash decodes
    it, `$"..."` read as `"..."`, and cut at its first NUL, since bash words
    are C strings -- or None when it holds an expansion (shell_has_glob's
    pathname and brace patterns included) or quoting _heredoc_delimiter()
    does not model."""
    if shell_has_expansion(node) or shell_has_glob(node, src):
        return None
    lit = _heredoc_delimiter(src[node.start_byte:node.end_byte], newlines=True, ansi=True)
    return None if lit is None else lit.split(b"\0", 1)[0]


def shell_has_glob(node, src: bytes) -> bool:
    """Whether bash expands a word as a pattern: an unquoted, unescaped `*`
    or `?`, a `[` with a `]` after it, or a `{` with a `,` or `..` after it
    (brace expansion) -- read over shell_unquoted_bytes()."""
    text = shell_unquoted_bytes(node, src)
    if b"*" in text or b"?" in text:
        return True
    k = text.find(b"[")
    if k >= 0 and b"]" in text[k + 1:]:
        return True
    k = text.find(b"{")
    return k >= 0 and (b"," in text[k + 1:] or b".." in text[k + 1:])


#: bash's `$'..'` single-character escapes.
SHELL_ANSI_ESCAPES = {
    b"a": b"\a", b"b": b"\b", b"e": b"\x1b", b"E": b"\x1b", b"f": b"\f", b"n": b"\n", b"r": b"\r",
    b"t": b"\t", b"v": b"\v", b"\\": b"\\", b"'": b"'", b'"': b'"', b"?": b"?",
}


def _ansi_c_body(word: bytes, i: int) -> Optional[tuple]:
    """Decode a `$'..'` body starting just after its opening quote:
    (bytes, index after the closing quote), or None for an escape bash's
    grammar has and this does not model (`\\c`, `\\u`/`\\U`) or an unclosed
    quote. Octal is 1-3 digits, `\\x` 1-2 hex digits."""
    out, n = bytearray(), len(word)
    while i < n:
        c = word[i:i + 1]
        if c == b"'":
            return bytes(out), i + 1
        if c != b"\\":
            out += c
            i += 1
            continue
        e = word[i + 1:i + 2]
        if e in SHELL_ANSI_ESCAPES:
            out += SHELL_ANSI_ESCAPES[e]
            i += 2
        elif e in b"01234567" and e:
            j = i + 1
            while j < n and j < i + 4 and word[j:j + 1] in b"01234567":
                j += 1
            out.append(int(word[i + 1:j], 8) & 0xFF)
            i = j
        elif e == b"x":
            j = i + 2
            while j < n and j < i + 4 and word[j:j + 1] in b"0123456789abcdefABCDEF":
                j += 1
            if j == i + 2:
                return None
            out.append(int(word[i + 2:j], 16))
            i = j
        else:
            return None
    return None


def shell_evaluated_texts(tree, src: bytes) -> Optional[list]:
    """The shell code the script hands an evaluator (SHELL_EVALUATORS, or a
    SHELL_INTERPRETERS shell after an option holding `c`), run directly or
    under a SHELL_WRAPPERS wrapper: per command, its words joined (what `eval`
    runs) and each word alone (what `trap`, `-c` or `alias` runs), so no
    option word -- `--`, or one taking an argument, `-o errexit` -- can hide
    the code from where it is read. None when any such text is not a
    literal: what it runs cannot be read here."""
    texts, stack = [], [tree.root_node]
    while stack:
        node = stack.pop()
        stack.extend(node.children)
        if node.type != "command":
            continue
        words = shell_command_words(node)
        if not words:
            continue
        names = [posixpath.basename((shell_literal(w, src) or b"").decode("utf-8", "replace")) for w in words]
        at = None
        if names[0] in SHELL_EVALUATORS or names[0] in SHELL_INTERPRETERS:
            at = 0
        elif names[0] in SHELL_WRAPPERS:
            at = next((i for i, n in enumerate(names) if i and (n in SHELL_EVALUATORS or n in SHELL_INTERPRETERS)), None)
        if at is None:
            continue
        name, rest = names[at], words[at + 1:]
        if name in SHELL_INTERPRETERS:
            opt = next((i for i, n in enumerate(names[at + 1:]) if n.startswith("-") and not n.startswith("--") and "c" in n), None)
            if opt is None:
                continue
            rest = rest[opt + 1:]
        parts = []
        for w in rest:
            lit = shell_literal(w, src)
            if lit is None:
                return None
            if name == "alias":
                lit = lit.split(b"=", 1)[1] if b"=" in lit else b""
            parts.append(lit)
        texts.append(b" ".join(parts))
        if len(parts) > 1:
            texts.extend(parts)
    return texts


def shell_echoes_source(tree, src: bytes) -> bool:
    """Whether bash's verbose mode, which prints every input line as it is
    read (comments included) to stderr, may be on: a `#!` line passing a
    flag with `v` or a token holding `verbose` (`--verbose`, `-o verbose`),
    or `set` / `shopt` -- direct or
    under a SHELL_WRAPPERS wrapper -- handed a `-...v...` option, the word
    `verbose`, or an option word that is not a literal."""
    first = src.split(b"\n", 1)[0]
    if first.startswith(b"#!"):
        for tok in first[2:].split()[1:]:
            if b"verbose" in tok or (tok.startswith(b"-") and not tok.startswith(b"--") and b"v" in tok):
                return True
    stack = [tree.root_node]
    while stack:
        node = stack.pop()
        stack.extend(node.children)
        if node.type != "command":
            continue
        words = shell_command_words(node)
        if not words:
            continue
        lits = [shell_literal(w, src) for w in words]
        names = [posixpath.basename((x or b"").decode("utf-8", "replace")) for x in lits]
        at = 0 if names[0] in ("set", "shopt") else next(
            (i for i, n in enumerate(names) if i and names[0] in SHELL_WRAPPERS and n in ("set", "shopt")), None)
        if at is None:
            continue
        for lit in lits[at + 1:]:
            if lit is None:  # an option word computed at run time
                return True
            if lit == b"--":
                break
            if lit == b"verbose" or (lit.startswith(b"-") and b"v" in lit[1:]):
                return True
    return False


def shell_self_read_reason(parser, tree, src: bytes, own: bytes) -> str:
    """The source_reader reason a script earns, or "": verbose mode
    (shell_echoes_source), a self-read, a possible
    self-read, or evaluated text (shell_evaluated_texts) that is not a literal,
    does not parse, or nests deeper than 3. Evaluated text is appended to the
    script and the whole is checked again, so a name in one and a reader in
    the other still meet."""
    seen, work = set(), src
    for _ in range(4):
        if shell_echoes_source(tree, work):
            return "source_reader:shell_verbose"
        if shell_self_reads(tree, work, own):
            return "source_reader:shell_self_read"
        if shell_may_self_read(tree, work, own):
            return "source_reader:possible_self_read"
        texts = shell_evaluated_texts(tree, work)
        if texts is None:
            return "source_reader:shell_evaluated_text_not_literal"
        new = [t for t in texts if t not in seen]
        if not new:
            return ""
        seen.update(new)
        work = work + b"\n" + b"\n".join(new) + b"\n"
        tree = parser.parse(work)
        if tree.root_node.has_error:
            return "source_reader:shell_evaluated_text_unparsed"
    return "source_reader:shell_evaluated_text_nested"


SHELL_EXPANSIONS = {"simple_expansion", "expansion", "command_substitution", "arithmetic_expansion",
                    "process_substitution"}


def shell_names_itself(tree, src: bytes, own: bytes = b"") -> bool:
    """Any shell_self_refs() node at all."""
    return bool(shell_self_refs(tree, src, own))


def shell_has_expansion(node) -> bool:
    stack = [node]
    while stack:
        n = stack.pop()
        if n.type == "raw_string":
            continue
        if n.type in SHELL_EXPANSIONS:
            return True
        stack.extend(n.children)
    return False


def shell_may_self_read(tree, src: bytes, own: bytes = b"") -> bool:
    """True when the script names its own path (any shell_self_refs() node)
    ANYWHERE and a text reader is handed an expanded argument,
    or a reading redirect (`<`, `<&`, `<>`, numbered or not) reads an
    expanded path. A reader under a wrapper counts, as in shell_self_reads(). The path can travel through a
    variable (`SELF="$0"; sed -n 2p "$SELF"`) that shell_self_reads() cannot
    follow, so this does not try to: conservative by design, it refuses the
    proof rather than trace data flow."""
    if not shell_names_itself(tree, src, own):
        return False
    stack = [tree.root_node]
    while stack:
        node = stack.pop()
        if shell_reading_redirect(node):
            if any(shell_has_expansion(ch) for ch in node.children):
                return True
        if node.type == "command" and shell_command_reads(node, src):
            if any(shell_has_expansion(ch) for ch in node.children
                   if ch.type not in ("command_name", "variable_assignment")):
                return True
        stack.extend(node.children)
    return False


def _heredoc_delimiter(word: bytes, newlines: bool = False, ansi: bool = False) -> Optional[bytes]:
    """The delimiter bash compares a heredoc's lines against: the word after
    quote removal (no expansion). Outside quotes `\\x` is `x`; inside single
    quotes everything is literal; inside double quotes a backslash is removed
    only before `$`, a backtick, `"`, `\\` or a newline and KEPT otherwise.
    Any other quoting -- `$'..'` (ANSI-C), `$"..."` (locale), an unclosed
    quote, a trailing backslash -- is not modelled: None, which the caller
    reads as a disagreement. Fail-closed. `newlines` reads a word that holds a
    newline (evaluated text), keeping it as written; `ansi` decodes `$'..'`
    (_ansi_c_body) and reads `$"..."` as `"..."` (shell_literal's words)."""
    if not newlines and (b"\n" in word or b"\r" in word):
        # A line continuation or a quoted newline inside the word: whether
        # bash removes it (`\<newline>`) or keeps it is not modelled.
        return None
    out, i, n = bytearray(), 0, len(word)
    while i < n:
        c = word[i:i + 1]
        if c == b"$" and word[i + 1:i + 2] == b"'" and ansi:
            got = _ansi_c_body(word, i + 2)
            if got is None:
                return None
            out += got[0]
            i = got[1]
            continue
        if c == b"$" and word[i + 1:i + 2] == b'"' and ansi:
            i += 1
            continue
        if c == b"$" and word[i + 1:i + 2] in (b"'", b'"'):
            return None
        if c == b"\\":
            if i + 1 >= n:
                return None
            out += word[i + 1:i + 2]
            i += 2
        elif c == b"'":
            j = word.find(b"'", i + 1)
            if j < 0:
                return None
            out += word[i + 1:j]
            i = j + 1
        elif c == b'"':
            i += 1
            while i < n and word[i:i + 1] != b'"':
                if word[i:i + 1] == b"\\" and i + 1 < n and word[i + 1:i + 2] in (b"$", b"`", b'"', b"\\", b"\n"):
                    out += word[i + 1:i + 2]
                    i += 2
                else:
                    out += word[i:i + 1]
                    i += 1
            if i >= n:
                return None
            i += 1
        else:
            out += c
            i += 1
    return bytes(out)


def shell_heredoc_disagrees(tree, src: bytes) -> bool:
    """True when tree-sitter-bash may end a here-document on a different line
    than bash does -- so a line it parses as a COMMENT may be heredoc DATA
    (or the reverse). bash ends a `<<` heredoc at the first line that is
    EXACTLY the delimiter (after quote removal, _heredoc_delimiter), and a `<<-` one
    at the first line that is the delimiter after stripping leading TABS
    only; a trailing space, a leading space, or a CR keeps the body open.
    Every heredoc must end where bash's rule says, on the line tree-sitter
    chose, with no earlier body line that bash would end it on; anything else
    -- no end, an unreadable delimiter -- is a disagreement. Fail-closed."""
    lines = src.split(b"\n")
    stack = [tree.root_node]
    while stack:
        node = stack.pop()
        stack.extend(node.children)
        if node.type != "heredoc_redirect":
            continue
        kids = {ch.type: ch for ch in node.children}
        start, end = kids.get("heredoc_start"), kids.get("heredoc_end")
        if start is None or end is None:
            return True
        dash = "<<-" in kids
        delim = _heredoc_delimiter(src[start.start_byte:start.end_byte])
        if not delim:
            return True

        def terminates(line: bytes) -> bool:
            return (line.lstrip(b"\t") if dash else line) == delim

        end_row = end.start_point[0]
        if end_row >= len(lines) or not terminates(lines[end_row]):
            return True
        body = kids.get("heredoc_body")
        first = body.start_point[0] if body is not None else end_row
        if any(terminates(lines[r]) for r in range(first, end_row)):
            return True
    return False


def js_source_texts(tree, src: bytes) -> list:
    """The text of every JS_SOURCE_TEXT_NODES node, in source order."""
    out, stack = [], [tree.root_node]
    while stack:
        node = stack.pop()
        if node.type in JS_SOURCE_TEXT_NODES:
            out.append(src[node.start_byte:node.end_byte])
        stack.extend(reversed(node.children))
    return out


def js_stem(path: str) -> str:
    """The basename without its last extension when that is a JS_EMIT_EXTS
    one (`a.test.ts` -> `a.test`), else the basename."""
    base = posixpath.basename(path)
    root, ext = posixpath.splitext(base)
    return root if ext in JS_EMIT_EXTS and root else base


def own_names(path: str, lang: str) -> list:
    """The spellings a literal can name this file by: its basename and, for
    the JS family, js_stem() under every JS_EMIT_EXTS extension."""
    base = posixpath.basename(path)
    names = {base}
    if lang in JS_FAMILY:
        names.update(js_stem(path) + ext for ext in JS_EMIT_EXTS)
    return sorted(n.encode("utf-8", "surrogateescape") for n in names if n)


def js_specifier_names(path: str) -> set:
    """The last path components an extensionless module specifier resolves
    to this file by: js_stem(), and for an `index` file its directory's name
    and `.` / `..` (a specifier naming the directory, `./`, `..`)."""
    stem = js_stem(path)
    out = {stem}
    if stem == "index":
        out.update({posixpath.basename(posixpath.dirname(path)), ".", ".."})
    return {n.encode("utf-8", "surrogateescape") for n in out if n}


def js_names_specifier(text: bytes, specifiers: set) -> bool:
    """Whether a string's last path component (a trailing `/` dropped) is one
    of `specifiers` (js_specifier_names())."""
    stripped = text.rstrip(b"/")
    return bool(stripped) and stripped.rsplit(b"/", 1)[-1] in specifiers


#: JS single-character escapes; any other `\\c` is `c`.
JS_ESCAPES = {b"n": "\n", b"t": "\t", b"r": "\r", b"b": "\b", b"f": "\f", b"v": "\v", b"0": "\0"}
#: Global objects whose property is the global of that name
#: (`globalThis.process` is `process`).
JS_GLOBAL_OBJECTS = {b"globalThis", b"global", b"window", b"self"}


def js_string_value(node, src: bytes) -> Optional[bytes]:
    """The value of a JS/TS `string`, or of a `template_string` with no
    substitution, with every escape decoded (`\\u0063`, `\\u{63}`, `\\x63`,
    a legacy octal `\\143`, a line continuation, `\\c` for `c`), as UTF-8 --
    a surrogate pair joined, and a lone surrogate or a source byte that is not
    UTF-8 written as U+FFFD, as node writes such a string to a path; a
    template's CR and CRLF read as LF, and a JSX attribute string's
    character references decoded as the JSX compilers decode them
    (jsx_unescape; self_naming also reads it as Babel collapses it,
    jsx_attribute_babel); None for any other node
    or a template that substitutes."""
    if node is None or node.type not in ("string", "template_string"):
        return None
    try:
        return _js_string_value(node, src)
    except (ValueError, OverflowError, UnicodeEncodeError):
        return None


def _js_string_value(node, src: bytes) -> Optional[bytes]:
    if node.type == "string" and node.parent is not None and node.parent.type == "jsx_attribute":
        # A JSX attribute string has no escapes and no comments, only
        # character references -- tree-sitter may still give it a
        # `comment` child (`c="//&amp;..."`), so its whole text is read.
        inner = src[node.start_byte + 1:node.end_byte - 1].decode("utf-8", "replace")
        return js_utf8(jsx_unescape(inner))
    out = []
    for ch in node.named_children:
        text = src[ch.start_byte:ch.end_byte]
        if ch.type == "string_fragment":
            if node.type == "template_string":
                # A template reads a CR or CRLF in its source as LF.
                text = text.replace(b"\r\n", b"\n").replace(b"\r", b"\n")
            out.append(text.decode("utf-8", "replace"))
        elif ch.type == "escape_sequence":
            body = text[1:]
            octal = re.match(rb"[0-3][0-7]{0,2}|[4-7][0-7]?", body)
            if octal:
                # Sloppy-mode legacy octal (`\143` is `c`): JS reads at most
                # two digits after a 4-7, so `\600` is `\60` then `0`.
                out.append(chr(int(octal.group(), 8)))
                out.append(body[octal.end():].decode("utf-8", "replace"))
                continue
            if body[:1] in (b"\n", b"\r") or body[:2] == b"\r\n" or body in ("\u2028".encode(), "\u2029".encode()):
                continue
            if body[:1] == b"u" and body[1:2] == b"{":
                out.append(chr(int(body[2:-1], 16)))
            elif body[:1] == b"u" and len(body) == 5:
                out.append(chr(int(body[1:], 16)))
            elif body[:1] == b"x" and len(body) == 3:
                out.append(chr(int(body[1:], 16)))
            elif body in JS_ESCAPES:
                out.append(JS_ESCAPES[body])
            else:
                out.append(body.decode("utf-8", "replace"))
        else:
            return None
    # JS strings are UTF-16: a `\uD83D\uDE00` pair is one code point. A
    # lone surrogate, like a source byte that is not UTF-8 (decoded above
    # with replacement, as node reads its source), is U+FFFD when the string
    # is written out as UTF-8 -- a path, say.
    text = "".join(out).encode("utf-16-le", "surrogatepass").decode("utf-16-le", "replace")
    return text.encode("utf-8")


def js_object_name(node, src: bytes) -> bytes:
    """The text an object expression names, seen through what does not change
    the object: parentheses, a comma operator's last operand, a TS `as` /
    `satisfies` / `!` / `<T>` assertion, and a JS_GLOBAL_OBJECTS prefix
    (`globalThis.process` names `process`)."""
    while node is not None:
        kind = node.type
        if kind == "parenthesized_expression" and node.named_child_count:
            node = node.named_children[-1]
        elif kind == "sequence_expression" and node.named_child_count:
            node = node.named_children[-1]
        elif kind in ("as_expression", "satisfies_expression", "non_null_expression") and node.named_child_count:
            node = node.named_children[0]
        elif kind == "type_assertion" and node.named_child_count:
            node = node.named_children[-1]
        elif kind in ("member_expression", "subscript_expression"):
            obj = node.child_by_field_name("object")
            key = js_static_key(node.child_by_field_name("property" if kind == "member_expression" else "index"), src)
            if obj is not None and key is not None and js_object_name(obj, src) in JS_GLOBAL_OBJECTS:
                return key
            break
        else:
            break
    if node is not None and node.type in JS_IDENTIFIER_TYPES:
        return js_identifier_name(node, src)
    return src[node.start_byte:node.end_byte] if node is not None else b""


#: tree-sitter JS/TS nodes that hold a name JS reads with its `\\u` escapes
#: decoded (`__file\\u006eame` is `__filename`).
JS_IDENTIFIER_TYPES = {
    "identifier", "property_identifier", "shorthand_property_identifier",
    "shorthand_property_identifier_pattern", "private_property_identifier",
}


def jsx_unescape(text: str) -> str:
    """A JSX character reference decoded as the JSX compilers (esbuild, tsc,
    Babel) decode it: a named one by the XHTML entity table
    (html.entities.name2codepoint plus XHTML's `&apos;`, so `&lang;` is U+2329, not HTML5's
    U+27E8), a numeric one straight to its code point (`&#x80;` is U+0080,
    with no HTML5 remap; hex only after a lowercase `x`, so `&#X41;` stays
    as written); anything else is kept as written."""
    def one(m):
        ref = m.group(1)
        try:
            if ref[:2] == "#x":
                return chr(int(ref[2:], 16))
            if ref[:1] == "#":
                return chr(int(ref[1:], 10))
        except (ValueError, OverflowError):
            return m.group(0)
        code = html.entities.name2codepoint.get(ref, 0x27 if ref == "apos" else None)
        return chr(code) if code is not None else m.group(0)
    return re.sub(r"&(#[xX][0-9a-fA-F]+|#[0-9]+|[A-Za-z][A-Za-z0-9]*);", one, text)


#: How each JSX compiler evaluates JSX text, as
#: (name, decode references first, tabs to spaces, line breaks, edge blanks):
#: Babel decodes first, turns every tab into a space and trims spaces at
#: CR/LF lines (@babel/types cleanJSXElementLiteralChild); tsc trims first,
#: at CR/LF/U+2028/U+2029 lines, by its isWhiteSpaceSingleLine set (NEL and
#: U+200B included); esbuild trims first by the JS whitespace set (no NEL).
JSX_TEXT_READINGS = (
    ("babel", True, True, "\r\n|\n|\r", " "),
    ("tsc", False, False, "\r\n|\n|\r|\u2028|\u2029",
     "\t\v\f \u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u200b\u202f\u205f\u3000\ufeff"),
    ("esbuild", False, False, "\r\n|\n|\r|\u2028|\u2029",
     "\t\v\f \u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u202f\u205f\u3000\ufeff"),
)


#: JavaScript's `\s`: what Babel's `/\n\s+/g` matches after a newline.
JS_WHITESPACE_RE = "[\t\n\v\f\r \u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000\ufeff]+"


def jsx_attribute_babel(value: bytes) -> bytes:
    """A JSX attribute string as Babel's JSX transform emits it: every LF
    followed by JS whitespace collapsed to one space (`convertAttribute`'s
    `value.replace(/\\n\\s+/g, " ")`); tsc and esbuild keep the value as
    written."""
    text = value.decode("utf-8", "replace")
    return re.sub("\n" + JS_WHITESPACE_RE, " ", text).encode("utf-8")


def jsx_reference_unmodelled(ref: bytes) -> bool:
    """Whether a JSX character-reference node (tree-sitter may include the
    blank before it) holds a numeric reference naming no Unicode code point
    (above U+10FFFF), where the compilers disagree."""
    for m in re.finditer(rb"&#(x[0-9a-fA-F]+|[0-9]+);", ref):
        digits = m.group(1)
        if (int(digits[1:], 16) if digits[:1] == b"x" else int(digits)) > 0x10FFFF:
            return True
    return False


def jsx_text_value(raw: str, lines_re: str, blanks: str, tabs: bool = False) -> str:
    """JSX text as one compiler evaluates it: split into lines at
    `lines_re`, every tab made a space when `tabs`, each line after the
    first stripped of leading `blanks` and each line before the last of
    trailing ones, empty lines dropped, the rest joined by one space
    (`<p>s<LF>   p.jsx</p>` is "s p.jsx")."""
    lines = re.split(lines_re, raw)
    kept = []
    for k, line in enumerate(lines):
        if tabs:
            line = line.replace("\t", " ")
        if k:
            line = line.lstrip(blanks)
        if k < len(lines) - 1:
            line = line.rstrip(blanks)
        if line:
            kept.append(line)
    return " ".join(kept)


def js_jsx_texts(node, src: bytes) -> list:
    """Each run of a JSX element's text children -- `jsx_text` and the
    character references JSX decodes in it (jsx_unescape: `j&#46;jsx` is
    `j.jsx`) -- as UTF-8, one run between child elements or expressions,
    decoded, and as each JSX_TEXT_READINGS compiler evaluates it."""
    # A run is read from the source between the siblings that bound it (the
    # opening tag or a child element before, the next child or the closing
    # tag after), so whitespace beside a reference -- which tree-sitter puts
    # in no node -- is kept.
    runs, start, has_text = [], None, False
    for ch in node.children:
        if ch.type in ("jsx_text", "html_character_reference"):
            has_text = True
            continue
        if has_text and start is not None:
            runs.append(src[start:ch.start_byte].decode("utf-8", "replace"))
        start, has_text = ch.end_byte, False
    out = []
    for raw in runs:
        out.append(js_utf8(jsx_unescape(raw)))
        for _name, decode_first, tabs, lines_re, blanks in JSX_TEXT_READINGS:
            if decode_first:
                value = jsx_text_value(jsx_unescape(raw), lines_re, blanks, tabs)
            else:
                value = jsx_unescape(jsx_text_value(raw, lines_re, blanks, tabs))
            out.append(js_utf8(value))
    return out


def js_utf8(text: str) -> bytes:
    """A JS string as node writes it to a path: UTF-16 surrogate pairs
    joined, a lone surrogate as U+FFFD."""
    return text.encode("utf-16-le", "surrogatepass").decode("utf-16-le", "replace").encode("utf-8")


def js_identifier_name(node, src: bytes) -> bytes:
    """An identifier's name with its `\\uXXXX` and `\\u{...}` escapes
    decoded, as UTF-8; the source text when an escape does not decode."""
    text = src[node.start_byte:node.end_byte]
    if b"\\" not in text:
        return text
    try:
        return re.sub(rb"\\u\{([0-9a-fA-F]+)\}|\\u([0-9a-fA-F]{4})",
                      lambda m: chr(int(m.group(1) or m.group(2), 16)).encode("utf-8"), text)
    except (ValueError, OverflowError, UnicodeEncodeError):
        return text


def js_static_key(node, src: bytes) -> Optional[bytes]:
    """A property key's name: a property identifier, a string or a plain
    template (js_string_value), or a `[ ]` computed key holding one; None
    when it is computed from anything else (a variable in `o[k]` or
    `{[k]: v}` is a value, not a name)."""
    if node is None:
        return None
    if node.type == "computed_property_name":
        inner = [ch for ch in node.named_children]
        return js_static_key(inner[0], src) if len(inner) == 1 else None
    if node.type in ("string", "template_string"):
        return js_string_value(node, src)
    if node.type in ("property_identifier", "shorthand_property_identifier_pattern", "private_property_identifier"):
        return js_identifier_name(node, src)
    return None


def js_property_reads(node, src: bytes) -> list:
    """The (object, property name) pairs a node reads: `o.p`, `o["p"]`
    and a destructuring `{p} = o` / `{["p"]: x} = o`, the object named by
    js_object_name(). The name is None when
    the key is computed and the object is one of JS_SELF_MEMBERS' objects,
    whose every property is then a possible self-name."""
    selfobjs = {o for o, _ in JS_SELF_MEMBERS}
    kind, out = node.type, []
    if kind in ("member_expression", "subscript_expression"):
        obj = node.child_by_field_name("object")
        key = node.child_by_field_name("property" if kind == "member_expression" else "index")
        otext = js_object_name(obj, src)
        name = js_static_key(key, src)
        if name is not None or otext in selfobjs:
            out.append((otext, name))
    elif kind == "variable_declarator" or kind == "assignment_expression":
        pat = node.child_by_field_name("name" if kind == "variable_declarator" else "left")
        val = node.child_by_field_name("value" if kind == "variable_declarator" else "right")
        if pat is not None and pat.type == "object_pattern":
            otext = js_object_name(val, src)
            for ch in pat.named_children:
                key = ch.child_by_field_name("key") if ch.type in ("pair_pattern", "object_assignment_pattern") else ch
                if ch.type == "object_assignment_pattern":
                    key = ch.child_by_field_name("left")
                name = js_static_key(key, src)
                if name is not None or otext in selfobjs:
                    out.append((otext, name))
    return out


def self_naming(lang: str, tree, src: bytes, names: list, specifiers: set = frozenset()) -> str:
    """The first spelling through which a JS/TS or Rust file names its own
    path or text ("" when none): JS_SELF_IDENTIFIERS, `import.meta`,
    JS_SELF_MEMBERS, JS_SELF_PROPERTIES, a module-level `arguments`, a string
    an extensionless specifier resolves here by (js_names_specifier());
    RUST_SELF_MACROS, RUST_SELF_NAMES; or, in any language, a non-comment
    leaf holding one of `names` (own_names())."""
    stack = [tree.root_node]
    while stack:
        node = stack.pop()
        kind = node.type
        if kind in COMMENT_TYPES:
            continue
        text = src[node.start_byte:node.end_byte]
        if node.child_count == 0 or kind in ("string_fragment", "string_content"):
            hit = next((n for n in names if n in text), None)
            if hit:
                return "literal:" + hit.decode("utf-8", "replace")
        if lang in JS_FAMILY:
            if kind in ("identifier", "shorthand_property_identifier") and js_identifier_name(node, src) in JS_SELF_IDENTIFIERS:
                return js_identifier_name(node, src).decode()
            # Every reading a compiler may give the string is tested on its
            # own: the specifier test is anchored at the string's end.
            value = js_string_value(node, src)
            readings = [] if value is None else [value]
            if value is not None and node.parent is not None and node.parent.type == "jsx_attribute":
                readings.append(jsx_attribute_babel(value))
            if value is None and kind in ("jsx_element", "jsx_fragment"):
                readings = js_jsx_texts(node, src)
            for reading in readings:
                hit = next((n for n in names if n in reading), None)
                if hit:
                    return "literal:" + hit.decode("utf-8", "replace")
                if js_names_specifier(reading, specifiers):
                    return "specifier:" + reading.decode("utf-8", "replace")
            if kind in ("identifier", "shorthand_property_identifier") and js_identifier_name(node, src) == b"arguments":
                up = node.parent
                while up is not None and up.type not in JS_ARGUMENTS_SCOPES:
                    up = up.parent
                if up is None:
                    return "arguments"
            if kind == "meta_property":
                return "import.meta"
            if kind == "html_character_reference" and jsx_reference_unmodelled(text):
                # esbuild decodes `&#x110000;` where tsc and Babel refuse it;
                # what it names cannot be read here, so it fails closed.
                return "jsx_reference_out_of_range"
            for obj, ptext in js_property_reads(node, src):
                if ptext is None:
                    return "computed:" + obj.decode("utf-8", "replace")
                if ptext in JS_SELF_PROPERTIES:
                    return ptext.decode()
                if (obj, ptext) in JS_SELF_MEMBERS:
                    return "%s.%s" % (obj.decode(), ptext.decode())
        elif lang == "rust":
            if kind == "macro_invocation":
                mac = node.child_by_field_name("macro")
                if mac is not None:
                    last = src[mac.start_byte:mac.end_byte].rsplit(b"::", 1)[-1].strip()
                    if last in RUST_SELF_MACROS:
                        return last.decode() + "!"
            if (node.child_count == 0 and text in RUST_SELF_MACROS and node.next_sibling is not None
                    and node.next_sibling.type == "!"):
                # Inside another macro's tokens (`println!("{}", file!())`)
                # tree-sitter keeps the invocation as bare tokens.
                return text.decode() + "!"
            if node.child_count == 0 or kind == "string_content":
                hit = next((n for n in RUST_SELF_NAMES if n in text), None)
                if hit:
                    return hit.decode()
        stack.extend(node.children)
    return ""


def rust_doc_whitelist(path: str, sides: tuple, rust_trees: Optional[dict]) -> tuple:
    """The gate for a CHANGED Rust doc group: (verdict, reason), NON_EXECUTABLE
    only when every changed group passes rust_doc_group_refusal() on both
    sides and, for an inner group, the parent `mod <name>;` of a non-root
    file is found and carries no doc fragment of its own (clause v)."""
    (rb, src_b), (rh, src_h) = sides
    for k, (db, dh) in enumerate(zip(rb.docs, rh.docs)):
        if db == dh:
            continue
        for red, src in ((rb, src_b), (rh, src_h)):
            why = rust_doc_group_refusal(red.doc_nodes[k], red.doc_kinds[k], src)
            if why:
                return EXECUTABLE, why
    for k, (db, dh) in enumerate(zip(rb.docs, rh.docs)):
        if db == dh or rb.doc_kinds[k] != "inner":
            continue
        if not rust_trees:
            return UNKNOWN, "rust_doc_parent_module_unread"
        for side in ("base", "head"):
            why = parent_module_docs(rust_trees[side], path)
            if why:
                return EXECUTABLE, "rust_doc_joined_parent_module:" + why
    return NON_EXECUTABLE, ""


def tree_sitter_arm(change: Change, ts: TreeSitter, rust_trees: Optional[dict] = None) -> None:
    lang = change.lang
    change.arm = lang
    if change.status != "M" or change.base is None or change.head is None:
        return
    if ts.state != "ok":
        change.verdict, change.reason = UNKNOWN, ts.reason
        return
    parser = ts.parser(lang)
    if parser is None:
        change.verdict, change.reason = UNKNOWN, "grammar_missing:" + lang
        return
    base, head = change.base, change.head
    if base.startswith(b"\xef\xbb\xbf") != head.startswith(b"\xef\xbb\xbf"):
        change.verdict, change.reason = EXECUTABLE, "bom_changed"
        return
    try:
        tb, th = parser.parse(base), parser.parse(head)
        rb, rh = reduce_tree(lang, tb, base), reduce_tree(lang, th, head)
    except Exception as exc:
        change.verdict, change.reason = UNKNOWN, "tree_sitter_failed:%s" % type(exc).__name__
        return
    if rb.has_error and rh.has_error:
        change.verdict, change.reason = UNKNOWN, "parse_error_both"
        return
    if rb.has_error or rh.has_error:
        change.verdict, change.reason = EXECUTABLE, "parse_error_" + ("base" if rb.has_error else "head")
        return
    if rb.tokens != rh.tokens:
        idx = next((i for i, (a, b) in enumerate(zip(rb.tokens, rh.tokens)) if a != b),
                   min(len(rb.tokens), len(rh.tokens)))
        tok = rh.tokens[idx] if idx < len(rh.tokens) else rb.tokens[min(idx, len(rb.tokens) - 1)]
        line = rh.lines[idx] if idx < len(rh.lines) else (rh.lines[-1] if rh.lines else 1)
        what = {"DIR": "directive_comment", "DOC": "rust_doc_presence", "NL": "line_break_between_tokens"}.get(
            tok[0] if tok else "", "token_change")
        change.verdict, change.reason = EXECUTABLE, "%s@%d" % (what, line)
        return
    if lang == "rust":
        # Kept ahead of the gate for its reason: an edit inside a doctest is
        # named as one. The gate below refuses every fence on its own.
        for db, dh in zip(rb.docs, rh.docs):
            if code_blocks(db) != code_blocks(dh):
                change.verdict, change.reason = EXECUTABLE, "rust_doc_code_block"
                return
        verdict, why = rust_doc_whitelist(change.path, ((rb, base), (rh, head)), rust_trees)
        if verdict != NON_EXECUTABLE:
            change.verdict, change.reason = verdict, why
            return
    if lang == "bash" and base != head and (shell_heredoc_disagrees(tb, base) or shell_heredoc_disagrees(th, head)):
        change.verdict, change.reason = EXECUTABLE, "shell_heredoc_terminator_ambiguous"
        return
    if lang in JS_FAMILY and base != head and js_source_texts(tb, base) != js_source_texts(th, head):
        change.verdict, change.reason = EXECUTABLE, "source_reader:js_function_source_text"
        return
    names = own_names(change.path, lang)
    if lang == "bash" and base != head:
        own = posixpath.basename(change.path).encode("utf-8", "surrogateescape")
        why = shell_self_read_reason(parser, tb, base, own) or shell_self_read_reason(parser, th, head, own)
        if why:
            change.verdict, change.reason = EXECUTABLE, why
            return
    elif base != head:
        specs = js_specifier_names(change.path) if lang in JS_FAMILY else frozenset()
        hit = self_naming(lang, tb, base, names, specs) or self_naming(lang, th, head, names, specs)
        if hit:
            change.verdict, change.reason = EXECUTABLE, "source_reader:self_named:" + hit
            return
    change.verdict, change.reason = NON_EXECUTABLE, "comments_and_whitespace_only"


# ---------------------------------------------------------------------------
# files with no language arm
# ---------------------------------------------------------------------------

def is_binary(blob: Optional[bytes]) -> bool:
    if blob is None:
        return False
    if b"\0" in blob:
        return True
    try:
        blob.decode("utf-8")
    except UnicodeDecodeError:
        return True
    return False


def text_doctest_changed(base: bytes, head: bytes) -> bool:
    """True when a changed paragraph (blank-line delimited) holds `>>>` on
    either side: a text file a repo collects as a doctest (`--doctest-glob`)."""
    if b">>>" not in base and b">>>" not in head:
        return False
    a = base.decode("utf-8", "replace").split("\n")
    b = head.decode("utf-8", "replace").split("\n")

    def blank(ln):
        # doctest's own rule: an example's expected output ends at a line of
        # SPACES only (tabs are expanded first). `str.strip()` would also eat
        # NBSP, U+2000-range spaces, CR and \x1c-\x1f, splitting a paragraph
        # doctest reads as one -- and hiding an edit to its expected output.
        return not ln.strip(" \t")

    def para(lines, lo, hi):
        while lo > 0 and not blank(lines[lo - 1]):
            lo -= 1
        while hi < len(lines) and not blank(lines[hi]):
            hi += 1
        return lines[lo:hi]

    for op, i1, i2, j1, j2 in difflib.SequenceMatcher(None, a, b, autojunk=False).get_opcodes():
        if op == "equal":
            continue
        if any(">>>" in ln for ln in para(a, i1, i2)) or any(">>>" in ln for ln in para(b, j1, j2)):
            return True
    return False


#: A Sphinx doctest-extension directive (reST `.. testsetup::`, MyST
#: ```` ```{testcode} ````). Its content need hold no `>>>` -- `testcode` is
#: plain code, and a `testsetup` group feeds every later example in the file
#: -- so no paragraph rule can localise an edit near one. Unanchored, so an
#: indented or quoted directive counts too.
#: Case-insensitive: docutils lowercases a directive name before it looks it
#: up, so `.. TESTSETUP::` is `testsetup` (round 10's B2).
SPHINX_DOCTEST_RE = re.compile(rb"(?:\.\.[ \t]*|\{)(?:doctest|testsetup|testcleanup|testcode|testoutput)[ \t]*(?:::|\})",
                               re.IGNORECASE)

#: Byte-order marks of the wide encodings a doctest runner can be told to
#: read (`pytest --doctest-encoding`, `doctest.testfile(encoding=)`).
_WIDE_BOMS = (b"\xff\xfe", b"\xfe\xff", b"\x00\x00\xfe\xff")
_WIDE_ENCODINGS = ("utf-16-le", "utf-16-be", "utf-32-le", "utf-32-be")


def text_doctest_marker(blob: Optional[bytes]) -> Optional[str]:
    """The doctest marker a text side carries, or None: `>>>` (a prompt
    doctest or pytest --doctest-glob collects), a Sphinx doctest directive,
    or `wide_encoding` -- a text that may be UTF-16/32 (a BOM, or NULs), in
    which a marker is not visible as bytes (round 10's N1). A blob that only
    LOOKS binary is still read: doctest reads a NUL-bearing file as text
    (round 10's G1), so no binary gate decides this."""
    if blob is None:  # G1: never gated on is_binary
        return None
    if SPHINX_DOCTEST_RE.search(blob):
        return "sphinx_doctest_directive"
    if b">>>" in blob:
        return "prompt"
    if blob.startswith(_WIDE_BOMS) or b"\0" in blob:
        for enc in _WIDE_ENCODINGS:
            text = blob.decode(enc, "ignore").encode("utf-8", "ignore")
            if SPHINX_DOCTEST_RE.search(text) or b">>>" in text:
                return "wide_encoding"
        if blob.startswith(_WIDE_BOMS):
            return "wide_encoding"
    return None


def code_paths_arm(change: Change, cp: CodePaths) -> None:
    """A file no language arm reads, judged by the declared code_paths."""
    change.arm = "code_paths"
    inside, why = cp.classify(change.path)
    if inside is None:
        change.verdict, change.reason = UNKNOWN, why
        return
    special = change.old_mode in (MODE_SYMLINK, MODE_GITLINK) or change.new_mode in (MODE_SYMLINK, MODE_GITLINK)
    if inside is False:
        # code_paths does not say what a doctest collector reads, so a text
        # that carries a doctest is never proven inert by lying outside them
        # (round 9's F5; D7's arming: a doctest-bearing edit is never
        # non_executable). A changed `>>>` paragraph is executable; an added
        # or deleted text with any marker is executable; any other edit to a
        # text carrying a Sphinx directive is unknown, since a `testsetup`
        # feeds examples far from it.
        if special:
            # A collector that walks with is_file() follows a link and reads
            # its target -- a doctest anywhere in the repository, or outside
            # it -- under this path, and walks into a submodule's tree (round
            # 10's B4). What a link or a submodule pointer names is not
            # proven inert by where the pointer itself lies.
            change.verdict, change.reason = UNKNOWN, (
                "symlink" if MODE_SYMLINK in (change.old_mode, change.new_mode) else "submodule") + "_outside_code_paths"
            return
        if change.status == "M" and change.base is not None and change.head is not None \
                and text_doctest_changed(change.base, change.head):
            change.verdict, change.reason = EXECUTABLE, "doctest_marker_in_text"
            return
        markers = [m for m in (text_doctest_marker(change.base), text_doctest_marker(change.head)) if m]
        if markers and change.status != "M":
            change.verdict, change.reason = EXECUTABLE, "doctest_marker_in_text:%s:%s" % (
                structural_reason(change), markers[0])
            return
        if "sphinx_doctest_directive" in markers or "wide_encoding" in markers:
            change.verdict, change.reason = UNKNOWN, "doctest_marker_in_text:" + (
                "sphinx_doctest_directive" if "sphinx_doctest_directive" in markers else "wide_encoding")
            return
        change.verdict, change.reason = NON_EXECUTABLE, "outside_code_paths"
        return
    ext = posixpath.splitext(change.path)[1].lower()
    if change.status != "M" or special or change.old_mode != change.new_mode:
        change.verdict, change.reason = EXECUTABLE, "inside_code_paths:" + structural_reason(change)
    elif ext in WHOLLY_SIGNIFICANT_EXTS:
        change.verdict, change.reason = EXECUTABLE, "inside_code_paths:no_inert_sublanguage"
    elif is_binary(change.base) or is_binary(change.head):
        change.verdict, change.reason = EXECUTABLE, "inside_code_paths:binary"
    else:
        change.verdict, change.reason = UNKNOWN, "inside_code_paths:no_grammar:" + (ext or "no_extension")


def wants_base(change: Change) -> bool:
    """Does judging this change need the base blob?"""
    return change.status in ("M", "T", "D") and change.old_sha != NULL_SHA and change.old_mode != MODE_GITLINK


def wants_head(change: Change) -> bool:
    """Does judging this change need the head side?"""
    return change.status in ("M", "T", "A") and change.new_mode != MODE_GITLINK


def is_special(change: Change) -> bool:
    """A symlink or a submodule on either side: its content is not source."""
    return (change.old_mode in (MODE_SYMLINK, MODE_GITLINK)
            or change.new_mode in (MODE_SYMLINK, MODE_GITLINK))


def structural_reason(change: Change) -> str:
    if change.status == "A":
        return "added"
    if change.status == "D":
        return "deleted"
    if MODE_GITLINK in (change.old_mode, change.new_mode):
        return "submodule"
    if MODE_SYMLINK in (change.old_mode, change.new_mode):
        return "symlink"
    if change.status == "T":
        return "type_changed"
    return "mode_changed"


# ---------------------------------------------------------------------------
# Rust includes: a non-code file a Rust crate compiles in
# ---------------------------------------------------------------------------

#: Rust's Pattern_White_Space: what its lexer skips between two tokens (the
#: Reference, "Whitespace"). Python's `\s` is wrong both ways: it misses
#: U+200E / U+200F, which Rust accepts between tokens -- so
#: `include_str!<U+200E>("x")` would hide an include from a `\s` regex -- and it
#: accepts NBSP and \x1c-\x1f, which Rust rejects. Every regex here that
#: tokenises Rust source spells whitespace RS_WS, and rust_segments() uses the
#: same set.
RUST_PATTERN_WHITE_SPACE = "\t\n\x0b\x0c\r \x85\u200e\u200f\u2028\u2029"
RS_WS = "[" + re.escape(RUST_PATTERN_WHITE_SPACE) + "]"
#: Whitespace and (non-nested) comments between two tokens.
RS_TRIVIA = r"(?:%s|//[^\n]*|/\*(?:[^*]|\*(?!/))*\*/)*" % RS_WS
#: `doc =` inside an attribute: a doc FRAGMENT (`#[doc = ".."]`,
#: `#[doc = include_str!(..)]`, `#[cfg_attr(x, doc = ..)]`).
DOC_FRAGMENT_ATTR_RE = re.compile(r"\bdoc" + RS_TRIVIA + "=(?!=)")
PATH_ATTR_RE = re.compile(r"\bpath" + RS_TRIVIA + "=(?!=)")
CLOSERS = {"(": ")", "[": "]", "{": "}"}


# ---- a coarse Rust lexer: enough to find an item's doc fragments --------------

TRIVIA_KINDS = ("ws", "comment", "doc_outer", "doc_inner", "attr_outer", "attr_inner")
_RS_RAW_STR_RE = re.compile(r'(?:b|c)?r(#*)"')
_RS_STR_RE = re.compile(r'(?:b|c)?"')
_RS_CHAR_RE = re.compile(r"'(?:\\(?:x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f_]{1,8}\}|[^\n])|[^\\\n'])'")


def _rs_is_ident(ch: str) -> bool:
    return ch == "_" or (ch.isascii() and ch.isalnum()) or (not ch.isascii() and ch not in RUST_PATTERN_WHITE_SPACE)


def _rs_block_comment_end(text: str, i: int) -> int:
    depth, k, n = 0, i, len(text)
    while k < n:
        if text.startswith("/*", k):
            depth += 1
            k += 2
        elif text.startswith("*/", k):
            depth -= 1
            k += 2
            if depth == 0:
                return k
        else:
            k += 1
    return n


def _rs_token(text: str, i: int) -> tuple:
    """(kind, end) of the one token or trivia run starting at text[i]."""
    n = len(text)
    c = text[i]
    if c in RUST_PATTERN_WHITE_SPACE:
        j = i + 1
        while j < n and text[j] in RUST_PATTERN_WHITE_SPACE:
            j += 1
        return "ws", j
    if text.startswith("//", i):
        j = text.find("\n", i)
        j = n if j < 0 else j
        if text.startswith("//!", i):
            return "doc_inner", j
        if text.startswith("///", i) and not text.startswith("////", i):
            return "doc_outer", j
        return "comment", j
    if text.startswith("/*", i):
        j = _rs_block_comment_end(text, i)
        if text.startswith("/*!", i):
            return "doc_inner", j
        if text.startswith("/**", i) and not text.startswith("/***", i) and not text.startswith("/**/", i):
            return "doc_outer", j
        return "comment", j
    m = _RS_RAW_STR_RE.match(text, i)
    if m:
        close = '"' + m.group(1)
        j = text.find(close, m.end())
        return "other", (n if j < 0 else j + len(close))
    m = _RS_STR_RE.match(text, i)
    if m:
        j = m.end()
        while j < n and text[j] != '"':
            j += 2 if text[j] == "\\" else 1
        return "other", min(j + 1, n)
    if c == "'" or text.startswith("b'", i):
        m = _RS_CHAR_RE.match(text, i + (1 if c == "b" else 0))
        if m:
            return "other", m.end()
        if c == "'":
            return "other", i + 1          # a lifetime's quote
    if _rs_is_ident(c):
        j = i + 1
        while j < n and _rs_is_ident(text[j]):
            j += 1
        return "other", j
    return "other", i + 1


def _rs_skip_trivia(text: str, k: int) -> int:
    while k < len(text):
        kind, j = _rs_token(text, k)
        if kind not in ("ws", "comment"):
            break
        k = j
    return k


def _rs_attribute(text: str, i: int) -> Optional[tuple]:
    """(kind, start, end) when an attribute `#[..]` / `#![..]` starts at i."""
    n = len(text)
    k = _rs_skip_trivia(text, i + 1)
    inner = k < n and text[k] == "!"
    if inner:
        k = _rs_skip_trivia(text, k + 1)
    if k >= n or text[k] != "[":
        return None
    depth = 0
    while k < n:
        kind, j = _rs_token(text, k)
        if kind == "other":
            t = text[k:j]
            if t in ("(", "[", "{"):
                depth += 1
            elif t in (")", "]", "}"):
                depth -= 1
                if depth == 0:
                    return ("attr_inner" if inner else "attr_outer", i, j)
        k = j
    return ("attr_inner" if inner else "attr_outer", i, n)


def rust_segments(text: str) -> list:
    """The source as (kind, start, end) segments: `ws`, `comment`,
    `doc_outer` / `doc_inner` (doc comments), `attr_outer` / `attr_inner`
    (whole attributes), and `other` (one token: an identifier, a literal, a
    punctuation character). String and char literals are single tokens, so a
    `//` or `#[` inside one is never read as a comment or an attribute."""
    segs = []
    i, n = 0, len(text)
    while i < n:
        if text[i] == "#":
            attr = _rs_attribute(text, i)
            if attr:
                segs.append(attr)
                i = attr[2]
                continue
        kind, j = _rs_token(text, i)
        segs.append((kind, i, j))
        i = j
    return segs


def _rs_is_doc_fragment(text: str, seg: tuple) -> bool:
    kind, s, e = seg
    if kind in ("doc_outer", "doc_inner"):
        return True
    return kind in ("attr_outer", "attr_inner") and bool(DOC_FRAGMENT_ATTR_RE.search(text, s, e))


def _rs_run(segs: list, idx: int) -> tuple:
    """The maximal run of trivia, doc comments and attributes around segs[idx]."""
    lo = hi = idx
    while lo > 0 and segs[lo - 1][0] in TRIVIA_KINDS:
        lo -= 1
    while hi + 1 < len(segs) and segs[hi + 1][0] in TRIVIA_KINDS:
        hi += 1
    return lo, hi


def _rs_file_inner_docs(text: str) -> bool:
    """True when the file's leading run holds an inner doc fragment."""
    for seg in rust_segments(text):
        if seg[0] not in TRIVIA_KINDS:
            return False
        if seg[0] in ("doc_inner", "attr_inner") and _rs_is_doc_fragment(text, seg):
            return True
    return False


class RustTree:
    """One side of the diff, read for Rust module resolution: the tree at a
    commit, or the working tree (rev None)."""

    def __init__(self, root: str, rev: Optional[str]):
        self.root, self.rev = root, rev
        self._paths = None
        self._cache = {}

    def paths(self) -> set:
        if self._paths is None:
            if self.rev:
                raw = git(self.root, "ls-tree", "-r", "-z", "--name-only", self.rev)
            else:
                raw = git(self.root, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
            self._paths = {p for p in raw.decode("utf-8", "surrogateescape").split("\0") if p}
        return self._paths

    def manifest_dirs(self) -> set:
        return {posixpath.dirname(p) for p in self.paths() if posixpath.basename(p) == "Cargo.toml"}

    def read(self, path: str) -> Optional[str]:
        """The file's RAW bytes as text: `git cat-file blob` (never `git show`,
        which may run a textconv filter) or the working-tree file."""
        if path not in self.paths():
            return None
        if path in self._cache:
            return self._cache[path]
        if self.rev:
            return git(self.root, "cat-file", "blob", "%s:%s" % (self.rev, path)).decode("utf-8", "replace")
        try:
            with open(os.path.join(self.root, *path.split("/")), "r", encoding="utf-8", errors="replace") as fh:
                return fh.read()
        except OSError:
            return None

    def prefetch(self, paths: list) -> None:
        """Read many blobs through ONE `git cat-file --batch` (a process per
        file is most of a scan's cost where spawning is slow). A path the
        batch cannot name (a newline in it) is left to read()."""
        if not self.rev:
            return
        want = [p for p in paths if p in self.paths() and p not in self._cache and "\n" not in p and "\r" not in p]
        if not want:
            return
        names = "".join("%s:%s\n" % (self.rev, p) for p in want).encode("utf-8", "surrogateescape")
        try:
            proc = subprocess.run(["git", "-C", self.root, *GIT_PIN, "cat-file", "--batch"], input=names,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        except OSError as exc:
            raise UsageError("cannot run git: %s" % exc) from exc
        if proc.returncode != 0:
            raise UsageError("git cat-file --batch failed (exit %d)" % proc.returncode)
        out, pos = proc.stdout, 0
        for p in want:
            eol = out.find(b"\n", pos)
            header = out[pos:eol].decode("ascii", "replace").split() if eol >= 0 else []
            if len(header) != 3 or header[1] != "blob":
                return      # anything unexpected: the rest go through read()
            size = int(header[2])
            start = eol + 1
            body = out[start:start + size]
            if len(body) != size:
                return
            self._cache[p] = body.decode("utf-8", "replace")
            pos = start + size + 1

    def symlinks(self) -> dict:
        """Every tracked symbolic link on this side: path -> target as
        stored (the blob of a mode-120000 entry, or the working-tree link).
        The working tree's also holds every link --others lists, which
        `git add -A` would track (round 25)."""
        if getattr(self, "_links", None) is None:
            links = {}
            if self.rev:
                raw = git(self.root, "ls-tree", "-r", "-z", self.rev)
                for ent in raw.split(b"\0"):
                    if not ent.startswith(b"120000 "):
                        continue
                    meta, _, name = ent.partition(b"\t")
                    path = name.decode("utf-8", "surrogateescape")
                    sha = meta.split()[2].decode("ascii")
                    links[path] = git(self.root, "cat-file", "blob", sha).decode("utf-8", "surrogateescape")
            else:
                raw = git(self.root, "ls-files", "-s", "-z")
                for ent in raw.split(b"\0"):
                    if not ent.startswith(b"120000 "):
                        continue
                    path = ent.partition(b"\t")[2].decode("utf-8", "surrogateescape")
                    try:
                        links[path] = os.readlink(os.path.join(self.root, *path.split("/")))
                    except OSError:
                        links[path] = "\0unreadable"
                raw = git(self.root, "ls-files", "-z", "--others", "--exclude-standard")
                for name in raw.split(b"\0"):
                    path = name.decode("utf-8", "surrogateescape")
                    full = os.path.join(self.root, *path.split("/"))
                    if name and os.path.islink(full):
                        links[path] = os.readlink(full)
            self._links = links
        return self._links

    def std_macro_shadows(self) -> tuple:
        """(names, all): the trusted std macro names (`_RS_EXPR_MACROS`) some
        .rs file in this tree may rebind -- a `macro_rules!` of that name, or a
        `use` naming it (`use x::documented as assert_eq;`, `use x::assert_eq;`)
        -- and whether ANY file carries `#[macro_use]`, which can put a
        same-named macro from another module or crate in scope with no `use`
        in the file that calls it (round 10's B3). Read over raw text, so a
        comment can only revoke trust, never grant it."""
        if getattr(self, "_shadows", None) is None:
            # EVERY tracked file, not only .rs: `include!("prelude.in")`
            # compiles a file of any name as Rust source (round 11).
            files = self.grep_rs(sorted(_RS_EXPR_MACROS) + ["macro_use"], whole_word=True, pathspec="*")
            self.prefetch(files)
            names, everything = set(), False
            for f in files:
                t = self.read(f) or ""
                if re.search(r"\bmacro_use\b", t):
                    # Any whole-word `macro_use`: `#[cfg_attr(all(),
                    # macro_use)]`, `#[/**/macro_use]` and paths to it are
                    # all spellings no attribute-shaped regex keeps up with.
                    everything = True
                for m in _RS_EXPR_MACROS:
                    if re.search(r"\bmacro(?:_rules\s*!)?\s+(?:r#)?%s\b" % m, t) \
                            or re.search(r"\bmacro_rules\s*!\s*(?:r#)?%s\b" % m, t) \
                            or re.search(r"\buse\b[^;]*\b%s\b" % m, t):
                        names.add(m)
            self._shadows = (names, everything)
        return self._shadows

    def grep_rs(self, words: list, whole_word: bool = False, pathspec: str = "*.rs") -> list:
        """The .rs files holding any of `words` (fixed strings). `-a` searches
        EVERY blob as text: without it git grep skips a blob it deems binary
        (a NUL byte, a `-diff` / `binary` attribute), and a compiled .rs file
        holding a NUL in a comment would hide its includes. `--no-textconv`
        searches the bytes git stores, never a filter's rendering.
        `--no-color`: a `color.grep=always` config wraps every name in escape
        codes, which would match no tracked path and drop the file silently."""
        args = ["grep", "-l", "-z", "-a", "--no-textconv", "--no-color", "-F"]
        if whole_word:
            args.append("-w")
        if not self.rev:
            args.append("--untracked")   # an option: it must precede the pattern
        for w in words:
            args += ["-e", w]
        if self.rev:
            args.append(self.rev)
        args += ["--", pathspec]
        out = git(self.root, *args, ok_codes=(0, 1), glob_pathspecs=True)   # `*.rs` is a glob
        files = []
        for raw in out.split(b"\0"):
            if not raw:
                continue
            name = raw.decode("utf-8", "surrogateescape")
            if self.rev:
                if not name.startswith(self.rev + ":"):
                    # Never drop a hit git reported: an unparsed name is no
                    # verdict, not a file with nothing in it.
                    raise UsageError("git grep answered an unparseable name %r" % name[:80])
                name = name[len(self.rev) + 1:]
            files.append(name)
        return files


def rust_is_crate_root(rs: str, manifest_dirs: set) -> bool:
    """A file Cargo compiles as a crate root BY ITS DEFAULT LAYOUT: no parent
    module can document it. Anything else (a custom `[lib] path` included) is
    treated as a module, whose parent is then looked up."""
    d, base = posixpath.split(rs)
    if base == "build.rs" and d in manifest_dirs:
        return True
    if base in ("lib.rs", "main.rs") and posixpath.basename(d) == "src" and posixpath.dirname(d) in manifest_dirs:
        return True
    parts = rs.split("/")
    for k in range(len(parts)):
        if "/".join(parts[:k]) not in manifest_dirs:
            continue
        rest = parts[k:]
        if rest[:2] == ["src", "bin"] and (len(rest) == 3 or (len(rest) == 4 and rest[3] == "main.rs")):
            return True
        if rest and rest[0] in ("tests", "examples", "benches") and (
                len(rest) == 2 or (len(rest) == 3 and rest[2] == "main.rs")):
            return True
    return False


#: Tokens a `mod` item's visibility may put before `mod`.
_RS_VIS_TOKENS = {"pub", "(", ")", "crate", "super", "self", "in", ":", "unsafe"}


def parent_module_docs(tree: RustTree, rs: str) -> Optional[str]:
    """Why the inner docs at the top of `rs` may be JOINED with outer docs on
    the `mod <name>;` that declares it, or None when they cannot be: a crate
    root has no parent, and a parent found with no doc fragment adds none.
    Unresolved is not None: a parent that cannot be found (a `#[path]`
    module, an unusual layout) may carry docs."""
    if rust_is_crate_root(rs, tree.manifest_dirs()):
        return None
    base = posixpath.basename(rs)
    name = posixpath.basename(posixpath.dirname(rs)) if base == "mod.rs" else posixpath.splitext(base)[0]
    if not name:
        return "parent_module_unresolved"
    found = False
    for decl in tree.grep_rs([name, base]):
        text = tree.read(decl)
        if text is None:
            return "parent_module_unreadable"
        segs = rust_segments(text)
        for seg in segs:
            if seg[0] == "attr_outer" and PATH_ATTR_RE.search(text, seg[1], seg[2]) and base in text[seg[1]:seg[2]]:
                return "path_attribute_module"
        others = [k for k, s in enumerate(segs) if s[0] == "other"]
        for pos, k in enumerate(others):
            if text[segs[k][1]:segs[k][2]] != "mod" or pos + 2 >= len(others):
                continue
            ident = text[segs[others[pos + 1]][1]:segs[others[pos + 1]][2]]
            if ident not in (name, "r#" + name) or text[segs[others[pos + 2]][1]:segs[others[pos + 2]][2]] != ";":
                continue
            found = True
            j = k - 1
            while j >= 0:
                kind, s, e = segs[j]
                if kind in TRIVIA_KINDS:
                    if kind in ("doc_outer", "attr_outer") and _rs_is_doc_fragment(text, segs[j]):
                        return "parent_outer_doc:" + decl
                elif text[s:e] in _RS_VIS_TOKENS:
                    pass
                elif text[s:e] in (";", "}", "{"):
                    break
                else:
                    return "parent_module_decl_unparsed:" + decl
                j -= 1
    return None if found else "parent_module_unresolved"


def doc_include_join(tree: RustTree, rs: str, text: str, segs: list, pos: int) -> Optional[str]:
    """Why the `#[doc = include_str!(..)]` at text[pos] may NOT be its item's
    only doc fragment, or None when it is. rustdoc joins the included file
    with every other fragment of the item -- a `///` / `//!` beside it,
    another `#[doc = ..]`, a module's inner docs with its outer docs -- so a
    fence opened in one fragment can continue into the included file, which
    the include arm judges alone. Conservative by design: ANY other fragment,
    whatever its text, refuses the proof."""
    idx = next((k for k, s in enumerate(segs) if s[1] <= pos < s[2]), None)
    if idx is None or segs[idx][0] not in ("attr_outer", "attr_inner"):
        return "include_not_in_an_attribute"
    lo, hi = _rs_run(segs, idx)
    # An outer fragment joins an outer include and an inner one an inner
    # include; the other kind beside it belongs to the enclosing module.
    same = ("doc_inner", "attr_inner") if segs[idx][0] == "attr_inner" else ("doc_outer", "attr_outer")
    for k in range(lo, hi + 1):
        if k != idx and segs[k][0] in same and _rs_is_doc_fragment(text, segs[k]):
            return "adjacent_doc_fragment"
    if segs[idx][0] == "attr_inner":
        if lo > 0:
            return "inner_doc_inside_a_block"
        return parent_module_docs(tree, rs)
    # An outer include documents the item after the run: a module's inner
    # docs join it.
    toks = []
    k = hi + 1
    while k < len(segs) and len(toks) < 16:
        kind, s, e = segs[k]
        if kind == "other":
            toks.append((text[s:e], k))
            if text[s:e] in (";", "{"):
                break
        elif kind not in ("ws", "comment"):
            break
        k += 1
    words = [t for t, _ in toks]
    if "mod" not in words:
        return None
    at = words.index("mod")
    if at + 2 >= len(words) or words[-1] not in (";", "{"):
        return "module_item_unparsed"
    name = words[at + 1][2:] if words[at + 1].startswith("r#") else words[at + 1]
    if words[-1] == "{":
        k = toks[-1][1] + 1
        while k < len(segs) and segs[k][0] in TRIVIA_KINDS:
            if _rs_is_doc_fragment(text, segs[k]):
                return "inline_module_inner_doc"
            k += 1
        return None
    if any(segs[k][0] == "attr_outer" and PATH_ATTR_RE.search(text, segs[k][1], segs[k][2]) for k in range(lo, hi + 1)):
        return "path_attribute_module"
    depth = sum(1 if text[s:e] == "{" else -1 if text[s:e] == "}" else 0
                for kind, s, e in segs[:idx] if kind == "other")
    if depth:
        return "nested_module_unresolved"
    d, base = posixpath.split(rs)
    if base not in ("mod.rs",) and not rust_is_crate_root(rs, tree.manifest_dirs()):
        d = posixpath.join(d, posixpath.splitext(base)[0])
    cands = [posixpath.join(d, name + ".rs"), posixpath.join(d, name, "mod.rs")]
    present = [c for c in cands if c in tree.paths()]
    if not present:
        return "module_file_unresolved"
    for c in present:
        sub = tree.read(c)
        if sub is None:
            return "module_file_unreadable"
        if _rs_file_inner_docs(sub):
            return "module_inner_doc:" + c
    return None


#: The std include family. A non-code file is non_executable only when no Rust
#: source in the repository could include it OPAQUELY: every occurrence of one
#: of these names must be a plain `name!("literal")` invocation, resolved to a
#: full repository path relative to the including file.
INCLUDE_NAMES = ("include", "include_str", "include_bytes")

#: Other compile-time file readers, by the identifier that names them. Any
#: occurrence in code makes every non-code candidate `unknown`: what they read
#: is decided by their own rules (a directory, a glob, a template root), never
#: by a literal this script can resolve. EXTENSIBLE: a reader added here is
#: refused everywhere; one missing from it is a stated boundary (D7).
OTHER_READER_IDENTS = {
    "include_dir",                       # include_dir!: a whole directory
    "include_flate",                     # include-flate (the crate path)
    "query_file", "query_file_as", "query_file_scalar",                         # sqlx
    "query_file_unchecked", "query_file_as_unchecked", "query_file_scalar_unchecked",
    "embed_migrations",                  # refinery, diesel_migrations
    "include_bytes_aligned",             # include_bytes_aligned!
    "RustEmbed", "rust_embed",           # rust-embed: #[derive(RustEmbed)] #[folder = ".."]
    "generate_context",                  # tauri::generate_context!(): tauri.conf.json, assets
    "include_wgsl", "include_spirv",     # wgpu shader includes
    "import_types", "generate_api",      # typify, progenitor: a schema / an API spec
    "include_scaffolding", "include_modules",   # uniffi, slint
    "static_loader", "dotenv",           # fluent-templates locales, dotenv_codegen
}
#: Readers named by a common word, refused only as a macro invocation.
OTHER_READER_MACROS = {"flate", "migrate", "i18n"}   # include-flate, sqlx::migrate!, rust-i18n
#: Readers spelled as attribute arguments: (attribute word, argument word);
#: None means `word = ...` directly in an attribute.
OTHER_READER_ATTR_ARGS = {
    ("template", "path"),                # askama, rinja, sailfish
    ("folder", None),                    # rust-embed
    ("grammar", None),                   # pest_derive
    ("schema_path", None), ("query_path", None),   # graphql_client
    ("module", None),                    # wasm_bindgen(module = "/x.js")
}

_RS_OPENERS = ("(", "[", "{")
#: Words before an identifier that make it a binding or field, not a macro.
_RS_BINDING_WORDS = {".", "fn", "let", "mut", "struct", "enum", "union", "mod", "const", "static", "type",
                     "trait", "ref"}
#: Rule B's RAW-TEXT scan, run over every scanned .rs file's whole text
#: (comments, string literals and attributes included) and over the whole text
#: of every file an attribute includes. rustdoc compiles doctests out of doc
#: text however it is spelled -- a `///` line, a `#[doc = "..."]` literal, a
#: README behind `#![doc = include_str!(..)]` -- so a name found anywhere but
#: at a CODE token (which the token scan judges) is OPAQUE. Blunt by design:
#: `include_str` / `include_bytes` count as bare words, `include` when a `!`
#: (or a comment) follows it; Rust's Pattern_White_Space counts as space.
_RAW_WS = "[\\s\\u200e\\u200f]"
_RAW_INCLUDE_RE = re.compile(r"\b(include_str|include_bytes)\b|\b(include)(?=%s*(?:!|/[/*]))" % _RAW_WS)
_RAW_READER_RE = re.compile(r"\b(%s)\b|\b(%s)(?=%s*!)" % (
    "|".join(sorted(OTHER_READER_IDENTS)), "|".join(sorted(OTHER_READER_MACROS)), _RAW_WS))
#: A reader spelled as an attribute argument, inside a comment or a string
#: (a doctest's `#[template(path = ..)]`).
_RAW_READER_ATTR_RE = re.compile(r"\b(%s)%s*(?:=(?!=)|\()" % (
    "|".join(sorted({w for w, _ in OTHER_READER_ATTR_ARGS})), _RAW_WS))
#: A `doc = <value>` whose value is not a plain string literal or an include:
#: `concat!(..)`, `stringify!(..)`, a macro's `$x`, a const -- doc text this
#: script cannot read, which may assemble an include out of pieces. It runs
#: over comment and string text (a doctest's `#[doc = ..]`), so it is
#: unanchored -- `f; doc = ..` handed to a macro counts -- and lets comments
#: sit on either side of the `=`. The CODE spelling is judged on the comment-
#: free token stream instead (`_doc_value_plain`), which nested comments
#: cannot hide from.
_RAW_TRIVIA = r"(?:%s|//[^\n]*|/\*(?:[^*]|\*(?!/))*\*/)*" % _RAW_WS
#: A `path = ..` meta in text -- `#[path = "x"]` or a `cfg_attr(.., path = ..)`
#: argument -- after `[`, `(` or `,`. In a doctest (`/// #[path = "data.txt"]
#: mod d;`) it compiles ANY file as Rust source, so it is opaque whatever it
#: names; prose that happens to read `(path = ..)` pays for that.
_RAW_PATH_ATTR_RE = re.compile(r"[\[(,]%s(path)%s=(?!=)" % (_RAW_TRIVIA, _RAW_TRIVIA))
_RAW_DOC_NON_LITERAL_RE = re.compile(
    r"\bdoc%s=(?!=)(?!%s(?:r?#*\"|include_str\b))" % (_RAW_TRIVIA, _RAW_TRIVIA))
#: The escapes a string literal decodes: `\x..`, `\u{..}` and a line
#: continuation. Doc text from `#[doc = "..."]` is the DECODED literal, so
#: an escaped underscore inside `include_str!` is still an include to rustdoc.
_RAW_ESCAPE_RE = re.compile(r"\\(?:x([0-9A-Fa-f]{2})|u\{([0-9A-Fa-f_]{1,8})\}|\r?\n%s*)" % _RAW_WS)


def _raw_unescape(text: str) -> str:
    def one(m):
        code = m.group(1) or (m.group(2) or "").replace("_", "")
        if not code:
            return ""
        try:
            return chr(int(code, 16))
        except (ValueError, OverflowError):
            return ""
    return _RAW_ESCAPE_RE.sub(one, text)


def _raw_names(text: str) -> list:
    """[(start, kind, name)] for every include-family or reader name in text."""
    found = []
    for m in _RAW_INCLUDE_RE.finditer(text):
        found.append((m.start(), "include", m.group(1) or m.group(2)))
    for m in _RAW_PATH_ATTR_RE.finditer(text):
        # Started at `path` itself, so a CODE `#[path = ..]` (judged by the
        # token scan) is excused by its token offset like any other name.
        found.append((m.start(1), "include", "path_attribute"))
    for m in _RAW_READER_RE.finditer(text):
        found.append((m.start(), "reader", m.group(1) or m.group(2)))
    for m in _RAW_READER_ATTR_RE.finditer(text):
        found.append((m.start(), "reader", "#[%s ..] in text" % m.group(1)))
    return found


def rust_raw_text_scan(rs: str, text: str, excused: set, scan: "IncludeScan", what: str) -> None:
    """Rule B over RAW text: every include-family or reader name not at an
    excused position (a code token the token scan already judged) is opaque,
    as is a non-literal `doc = ..` anywhere and a name only an escape
    decoding reveals."""
    names = _raw_names(text)
    for start, kind, name in names:
        if start in excused:
            continue
        if kind == "include":
            scan.opaque.append((rs, "%s:%s" % (what, name)))
        else:
            scan.readers.append((rs, name))
    decoded = _raw_unescape(text)
    if decoded != text and len(_raw_names(decoded)) > len(names):
        scan.opaque.append((rs, what + ":escaped_name"))
    if _RAW_DOC_NON_LITERAL_RE.search(text) or (decoded != text and _RAW_DOC_NON_LITERAL_RE.search(decoded)):
        scan.opaque.append((rs, what + ":doc_attribute_non_literal"))


def _rs_flat_tokens(text: str) -> tuple:
    """([(token, start)], [doc comment text]) -- every code token, attribute
    internals included; comments and whitespace dropped."""
    toks, docs = [], []
    i, n = 0, len(text)
    while i < n:
        kind, j = _rs_token(text, i)
        if kind == "other":
            toks.append((text[i:j], i))
        elif kind in ("doc_outer", "doc_inner"):
            docs.append(text[i:j])
        i = j
    # A raw identifier `r#name` IS the identifier `name` (round 12's F2): the
    # lexer above splits it into `r`, `#`, `name`, and every rule that reads
    # a name off the tokens (a `macro_rules!` / `macro` rebinding, a
    # `use .. as`, a macro's own name, an include or reader name) would read
    # `r`. Fold the three into the bare name here, once, for every consumer.
    out, k = [], 0
    while k < len(toks):
        t, at = toks[k]
        if t == "r" and k + 2 < len(toks) and toks[k + 1] == ("#", at + 1) and toks[k + 2][1] == at + 2 \
                and re.match(r"[A-Za-z_]", toks[k + 2][0]):
            out.append(toks[k + 2])     # the name's own offset: a raw-text scan excuses it
            k += 3
            continue
        out.append((t, at))
        k += 1
    return out, docs


def path_spelling_kind(p: str) -> Optional[str]:
    """How a path written in a commit is spelled, for EVERY site that
    resolves one (an include literal, a symlink target), so the two can
    never disagree about what is relative (round 14's F2). None: a plain
    POSIX relative path, the only spelling resolved component by component.
    "posix_absolute": a leading `/` with no backslash. "foreign": anything
    a Windows or UNC resolver reads differently -- any backslash (`\\x`
    is rooted, `\\\\host\\share` is UNC), or a drive letter -- which is
    never followed as a relative path."""
    if "\\" in p or re.match(r"[A-Za-z]:", p):
        return "foreign"
    if p.startswith("/"):
        return "posix_absolute"
    return None


def _rs_plain_literal(tok: str) -> Optional[str]:
    """The value of a plain string literal token that names a relative path,
    or None: an escape, a byte or C string, an absolute or drive path, or a
    backslash all make the path something this script does not resolve."""
    m = re.fullmatch(r'r(#*)"(.*)"\1', tok, re.DOTALL)
    if m:
        value = m.group(2)
    elif len(tok) >= 2 and tok[0] == '"' and tok[-1] == '"' and "\\" not in tok[1:-1]:
        value = tok[1:-1]
    else:
        return None
    if not value or path_spelling_kind(value) is not None:
        return None
    return value


def _rs_is_str_token(tok: str) -> bool:
    """A plain (or raw) string literal token: `".."` or `r#".."#`. A byte
    or C string is never doc text, so it does not count."""
    return tok.startswith('"') or bool(re.match(r'r#*"', tok))


#: What may follow a doc value: the end of the attribute, the next meta, the
#: end of a macro's arguments or of a statement.
_DOC_VALUE_ENDS = (",", ")", "]", "}", ";")


def _doc_value_plain(words: list, k: int, match: dict) -> bool:
    """For a `doc` token at k followed by `=`: True only when the value is a
    single plain string literal, or a plain `include_str!("literal")`, and
    nothing follows it but a delimiter. Anything else -- `concat!`, a `$x`,
    a qualified include, a const, a suffix -- is doc text this script cannot
    read. Fail-closed: a value it cannot classify is not plain."""
    n = len(words)
    v = k + 2
    if v >= n:
        return False
    if _rs_is_str_token(words[v]):
        end = v + 1
    elif words[v] == "include_str" and v + 2 < n and words[v + 1] == "!" and words[v + 2] in _RS_OPENERS \
            and match.get(v + 2) is not None:
        close = match[v + 2]
        args = words[v + 3:close]
        if not args or not _rs_is_str_token(args[0]) or args[1:] not in ([], [","]):
            return False
        end = close + 1
    else:
        return False
    return end < n and words[end] in _DOC_VALUE_ENDS


#: Standard macros whose arguments are expressions they never turn into an
#: attribute: a plain include handed to one of these (unqualified, and not
#: rebound anywhere: see `std_macro_shadows`) is read as data. Any
#: other macro may turn its argument into doc text, so an include inside its
#: arguments is opaque.
_RS_EXPR_MACROS = {
    "assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne",
    "print", "println", "eprint", "eprintln", "format", "format_args", "write", "writeln",
    "panic", "vec", "dbg", "todo", "unimplemented", "unreachable",
}


_RS_SIMPLE_ESCAPES = {"n": "\n", "r": "\r", "t": "\t", "\\": "\\", "0": "\0", "'": "'", '"': '"'}


def _rs_str_value(tok: str) -> str:
    """The text a plain or raw string literal token denotes. An escape this
    does not decode is kept verbatim, so nothing is lost from the scan."""
    m = re.fullmatch(r'r(#*)"(.*)"\1', tok, re.DOTALL)
    if m:
        return m.group(2)
    body = tok[1:-1] if len(tok) >= 2 and tok.endswith('"') else tok[1:]
    out, i, n = [], 0, len(body)
    while i < n:
        c = body[i]
        if c != "\\" or i + 1 >= n:
            out.append(c)
            i += 1
            continue
        e = body[i + 1]
        if e in _RS_SIMPLE_ESCAPES:
            out.append(_RS_SIMPLE_ESCAPES[e])
            i += 2
        elif e == "\n" or (e == "\r" and body[i + 2:i + 3] == "\n"):
            # A line continuation drops the newline and the next line's
            # leading whitespace.
            i += 2 if e == "\n" else 3
            while i < n and body[i] in RUST_PATTERN_WHITE_SPACE:
                i += 1
        else:
            m = _RAW_ESCAPE_RE.match(body, i)
            if m:
                out.append(_raw_unescape(m.group(0)))
                i = m.end()
            else:
                out.append(c)
                i += 1
    return "".join(out)


def _rs_doc_comment_texts(text: str) -> list:
    """Every doc text the doc comments of one Rust source spell. A run of doc
    fragments with only trivia and attributes between them is ONE item's doc
    to rustdoc, joined in order, so each run is returned joined; its outer-
    only and inner-only joins are returned too, since which fragments one
    item owns is not modelled. A block doc comment is returned verbatim and
    star-stripped. Over-reading is safe: every text is only scanned."""
    texts = []
    run = []                                   # (kind, body)

    def flush():
        if run:
            for keep in (("doc_outer", "doc_inner"), ("doc_outer",), ("doc_inner",)):
                part = [b for k, b in run if k in keep]
                if part:
                    texts.append("".join(part))
            run.clear()

    for kind, s, e in rust_segments(text):
        seg = text[s:e]
        if kind in ("doc_outer", "doc_inner"):
            if seg.startswith("//"):
                run.append((kind, seg[3:] + "\n"))
            else:
                body = seg[3:-2] if seg.endswith("*/") else seg[3:]
                run.append((kind, body + "\n"))
                texts.append("\n".join(re.sub(r"^[ \t]*\*", "", ln) for ln in body.split("\n")))
        elif kind in ("attr_outer", "attr_inner"):
            if DOC_FRAGMENT_ATTR_RE.search(seg):
                lits = [t for t, _ in _rs_flat_tokens(seg)[0] if _rs_is_str_token(t)]
                k = "doc_inner" if kind == "attr_inner" else "doc_outer"
                run.extend((k, _rs_str_value(t) + "\n") for t in lits)
        elif kind not in TRIVIA_KINDS:
            flush()
    flush()
    return texts


#: rustdoc's code-block attributes (`LangString::parse`); `_fence_is_rust`
#: applies them in rustdoc's order. Fail-closed: a word that only
#: LOOKS like an attribute (`ignore-wasm`, `edition2030`, `E0425`, a class or
#: a `key=value`) counts as one, so the block is scanned.
_RUSTDOC_BLOCK_ATTRS = {"rust", "ignore", "should_panic", "no_run", "compile_fail", "test_harness",
                        "standalone_crate", "allow_fail", "unstable", "custom", "standalone"}
_RUSTDOC_ATTR_RE = re.compile(r"(?:ignore-.*|edition\d*|E\d{4}|[.{}].*|.*=.*)$")

#: Doctests within doctests (a doc comment inside a doctest's code) are
#: scanned this many levels deep; deeper code is opaque.
DOCTEST_MAX_DEPTH = 3


def _fence_is_rust(info: str) -> bool:
    """MAY rustdoc compile this fenced block? A conservative SUPERSET of
    `LangString::parse`, never an exact copy: False only when rustdoc
    certainly skips the block. It follows rustdoc's ORDER (round 10's B1):
    `ignore`/`no_run`/`should_panic` and their kin set `seen_rust_tags =
    !seen_other_tags`, so one BEFORE the first unknown word keeps the block
    Rust (`no_run,sh`, `should_panic json`) and one after it does not
    (`sh,no_run`). Everything rustdoc's grammar has beyond a plain word
    list fails closed -- any character outside `[A-Za-z0-9_.=:-]`, comma and
    whitespace makes the block Rust: `{..}` attribute groups, `(..)`
    comments, quoted tokens, CommonMark backslash escapes and entity
    references (`&#114;ust`, `no_run\\,sh`) (round 11's F1). A
    case-variant of `rust` counts as `rust`."""
    if re.search(r"[^A-Za-z0-9_.=:,\s-]", info):
        return True
    words = [w for w in re.split(r"[,\s]+", info) if w]
    seen_other = False
    for w in words:
        lw = w.lower()
        if lw == "rust":
            return True
        if lw in _RUSTDOC_BLOCK_ATTRS or _RUSTDOC_ATTR_RE.match(w):
            if not seen_other:
                return True
        else:
            seen_other = True
    return not seen_other


def _dehide(line: str) -> str:
    """A doctest line as rustdoc compiles it: `# x` (hidden) is `x`, a lone
    `#` is empty, `##x` is `#x`."""
    t = line.lstrip(" \t")
    if t.startswith("##"):
        return t[1:]
    if t == "#" or t.startswith("# ") or t.startswith("#\t"):
        return t[2:]
    return line


def doctest_sources(text: str) -> list:
    """The Rust code rustdoc may compile out of one Markdown doc text: every
    fenced block it reads as Rust and every indented block, hidden lines
    shown. Where block context makes WHICH lines are code ambiguous (a quote,
    a list, an HTML block), the text is also read with `>` markers stripped
    and with every line dedented, and the union is returned."""
    variants = [text]
    if doc_layout_ambiguous(text):
        lines = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
        unquoted = [re.sub(r"^(?:[ \t]*>[ ]?)+", "", ln) for ln in lines]
        variants += ["\n".join(unquoted), "\n".join(ln.lstrip(" \t") for ln in unquoted)]
    sources = []
    for variant in variants:
        cur, indented, in_rust = [], [], False
        for entry in code_blocks(variant):
            if entry[0] == "open":
                in_rust, cur = _fence_is_rust(entry[1]), []
            elif entry[0] == "code":
                cur.append(_dehide(entry[1]))
            elif entry[0] in ("close", "unclosed"):
                if in_rust:
                    sources.append("\n".join(cur) + "\n")
                cur, in_rust = [], False
            elif entry[0] == "indented":
                indented.append(_dehide(entry[1]))
        if indented:
            sources.append("\n".join(indented) + "\n")
    return sources


class _NoTree:
    """A tree with no files: a doctest's include resolves against nothing
    this script can name, so every include in one is opaque."""

    def std_macro_shadows(self) -> tuple:
        return set(), False

    def paths(self) -> set:
        return set()

    def read(self, path: str):
        return None


def tree_symlinks(tree) -> dict:
    """A tree's tracked symlinks; a tree with no files has none."""
    f = getattr(tree, "symlinks", None)
    return f() if f else {}


#: Characters simple uppercase merges with another letter while
#: `str.casefold` keeps them apart: U+0131 DOTLESS I upper-cases to `I`, as
#: `i` does. Enumerated over every code point (round 17): it is the only one.
#: The $UpCase table mkntfs writes leaves U+0131 alone (round 21), so on NTFS
#: the map only over-joins, which fails closed.
_FOLD_EXTRA = str.maketrans({"\u0131": "i"})


#: Default_Ignorable_Code_Point, copied from Unicode 15.1.0's
#: DerivedCoreProperties.txt (the version `unicodedata` carries here; the
#: utf8-12.1 table e2fsprogs 1.47.2 builds ext4/f2fs casefolding from is the
#: same set less U+180F). Python has no property for it. It also holds the
#: 13.0, 16.0 and 17.0 sets (round 21), and from 13.0 through 17.0 no code
#: point was assigned before it became ignorable. Unicode has no stability
#: policy for the property, so a later version may do that, as 6.3 did to
#: U+180E; RESIDUALS says so (round 22). ext4/f2fs casefolding emits nothing
#: for one, so `con` U+200D `fig` is `config` there (round 20), and one still
#: ends the run of marks being reordered. The key
#: drops them FIRST, so U+0345 and a mark on either side of one meet; that
#: also lets the marks around one reorder, which ext4 does not do, and
#: over-joining only fails closed.
_DEFAULT_IGNORABLE = (
    (0x00AD, 0x00AD), (0x034F, 0x034F), (0x061C, 0x061C), (0x115F, 0x1160),
    (0x17B4, 0x17B5), (0x180B, 0x180F), (0x200B, 0x200F), (0x202A, 0x202E),
    (0x2060, 0x206F), (0x3164, 0x3164), (0xFE00, 0xFE0F), (0xFEFF, 0xFEFF),
    (0xFFA0, 0xFFA0), (0xFFF0, 0xFFF8), (0x1BCA0, 0x1BCA3), (0x1D173, 0x1D17A),
    (0xE0000, 0xE0FFF),
)
_DROP_IGNORABLE = {cp: None for lo, hi in _DEFAULT_IGNORABLE for cp in range(lo, hi + 1)}


#: U+03B9 GREEK SMALL LETTER IOTA. U+0345 COMBINING GREEK YPOGEGRAMMENI
#: case-folds to it, and that is the one case mapping that changes a combining
#: class (240 to 0, a starter), enumerated over every code point (round 19).
_IOTA = "\u03b9"


def _iota_rank(ch: str) -> int:
    """Sort rank inside a run of marks: an iota after every mark."""
    return 256 if ch == _IOTA else unicodedata.combining(ch)


def _iota_last(s: str) -> str:
    """`s` with every run of combining marks and iotas sorted by combining
    class, iotas last. D145 reorders U+0345 behind the marks before it folds
    it to an iota; ext4 and simple uppercase fold it where it stands. With
    the iota's place fixed, every one of those spellings is one key."""
    out, run = [], []
    for ch in s:
        if ch == _IOTA or unicodedata.combining(ch):
            run.append(ch)
        else:
            out += sorted(run, key=_iota_rank)
            run = []
            out.append(ch)
    return "".join(out + sorted(run, key=_iota_rank))


def fold_key(path: str) -> str:
    """A key that is EQUAL for every two spellings any modelled filesystem
    makes one entry, so two tracked spellings with one key may be ONE file
    there (round 15's F2), for names made of code points this interpreter's
    Unicode assigns. A name holding an unassigned one is `wild_kind`'s
    (round 21): only through such a code point can a later Unicode fold or
    normalise two names together. The models: Unicode D145 canonical caseless matching (the shape
    of a normalising, case-insensitive filesystem such as APFS); ext4/f2fs
    casefolding as e2fsprogs 1.47.2's utf8-12.1 table does it (each
    character's NFD casefold, then canonical ordering of each run of marks,
    where a Default_Ignorable code point emits nothing and ends the run),
    measured against that table with `e2fsck`, not against a kernel; and
    per-unit simple uppercase, a superset of the $UpCase table mkntfs writes
    (that table leaves U+0131, U+0345, U+00B5, U+017F, U+03C2 and U+1FBE
    alone). Each component is also stripped of the trailing dots and spaces
    Windows drops. The key joins more than any one model: pairs whose
    models' joins chain together (`ı̀`, U+0131 U+0300, is `I` U+0300 under
    simple uppercase, which D145 makes `ì`), and pairs no chain joins (`con`
    U+180F `fig` with `config`: U+180F is ignorable only from Unicode 14.0,
    after e2fsprogs' 12.1 table). Over-joining only fails closed.

    The steps: drop the ignorables, then D145 with `_FOLD_EXTRA` inside it
    (NFD, casefold, map, NFD), then `_iota_last`. Dropping the ignorables
    FIRST is load-bearing (round 20): ext4 drops them, so `a` U+0345 U+200D
    U+0301 is `aί` there, and a key that dropped them only at the end would
    keep U+0345 and U+0301 apart. Decomposing is load-bearing (round 18:
    an NFC-then-casefold key splits `ΐ`, U+0390, from `Ϊ́`, U+03AA U+0301),
    but only once: D145 decomposes before and after casefold, and either NFD
    alone gives the same keys. `_iota_last` is load-bearing (round 19): `a`
    U+0345 U+0301 is `aί` (U+03AF) on ext4 and `a` U+03B9 U+0301 under simple
    uppercase, while D145 alone gives `a` U+0301 U+03B9; its sort of the marks
    is load-bearing too (round 20: `a` U+0301 U+0345 U+0316 is `a` U+0301
    U+03B9 U+0316 on ext4 and under simple uppercase). Where the U+0131 map
    sits changes no key.

    Round 20 enumerated the key two ways, and it split no class of any of
    the three models. By class: every code point; each case-changing
    letter's case variants and their partial compositions; each
    case-changing letter before each BMP mark of class 200 or more or one
    of eight named marks; `a`, `a` U+03B9, `a` U+0399 and `a` U+0345 before
    each BMP mark; every string of one to four drawn from 29 interacting
    letters, marks, ignorables and `/`, and `x` before every four of the
    first sixteen; and `con`, an ignorable, `fig`. Per string, checking the
    key of x against the key of the model's image of x: each case-changing
    letter before every mark in every plane, and beside U+0345 and
    ignorables with one mark of each class, plus 2,000,000 seeded random
    strings of two to seven Greek letters, marks and ignorables. Round 21
    added two real tables. xnu's casefold (APFS, at Unicode 16.0, compiled
    from vfs_unicode.c): every pair of names it joins over every code point,
    and over strings of interacting letters, marks and ignorables, either
    gets one key or holds an unassigned code point. And the $UpCase table
    mkntfs writes: all 973 of its mappings are simple uppercase."""
    folded = path.translate(_DROP_IGNORABLE)
    folded = unicodedata.normalize("NFD", folded).casefold().translate(_FOLD_EXTRA)
    folded = _iota_last(unicodedata.normalize("NFD", folded))
    return "/".join(c.rstrip(". ") for c in folded.split("/"))


#: An NTFS 8.3 short name as a path component: a base of one to six
#: characters, `~` and digits, then an optional extension of one to three, in
#: any case, once Windows has dropped trailing dots and spaces. On a volume
#: that keeps short names, `CONFIG~1` can BE the directory `CONFIGURATION`
#: (round 21). The base may hold `~` itself (round 22): `~$settings.yaml` gets
#: `~$SETT~1.YAM`, and `a~bcdefgh` gets `A~BCDE~1`. When Windows makes one it
#: also drops spaces and extra dots and writes `_` for `+,;=[]` and for a
#: character outside the OEM code page, so a real short name holds none of
#: those; the match does not ask. Over-matching only fails closed.
_SHORT_NAME = re.compile(r"[^./]{1,6}~[0-9]+(?:\.[^./]{1,3})?", re.IGNORECASE)


def wild_kind(component: str) -> Optional[str]:
    """Why one path component may name the same entry as a sibling whose fold
    key differs, or None (round 21). `unassigned_codepoint`: it holds a code
    point this interpreter's Unicode leaves unassigned (category Cn). A later
    version may case-fold, decompose or ignore it, and a later version can
    fold or normalise two names together ONLY through such a code point: the
    case folding, decomposition and combining class of an assigned code
    point never change (Unicode's stability policies; checked between 13.0,
    15.1, 16.0 and 17.0). Default_Ignorable has no such policy; see
    RESIDUALS. xnu's casefold, at 16.0, joins `confꟋg` with `confɤg` (U+A7CB
    with U+0264). `undecodable`: a byte that is not UTF-8, which the checker
    reads as a lone surrogate (round 23's F3). Git for Windows writes an
    invalid byte of 0xA0 or more as U+00A0-U+00FF and one of 0x80-0x9F as two
    hex digits, so `app<E9>.yaml` IS `appé.yaml` there and `x<81>.yaml` IS
    `x81.yaml`. `short_name`: shaped like an NTFS 8.3 short name."""
    if any(unicodedata.category(ch) == "Cn" for ch in component):
        return "unassigned_codepoint"
    if any(unicodedata.category(ch) == "Cs" for ch in component):
        return "undecodable"
    if _SHORT_NAME.fullmatch(component.rstrip(". ")):
        return "short_name"
    return None


def tree_wild(tree) -> tuple:
    """(fold key of a directory -> every tracked entry in it, fold key of a
    directory -> the tracked entries in it whose own name is wild)."""
    if getattr(tree, "_wild", None) is None:
        entries, wild = {}, {}
        for p in set(tree.paths()) | tree_dirs(tree):
            parent, _, name = p.rpartition("/")
            entries.setdefault(fold_key(parent), set()).add(p)
            if wild_kind(name):
                wild.setdefault(fold_key(parent), set()).add(p)
        try:
            tree._wild = (entries, wild)
        except AttributeError:
            return entries, wild
    return tree._wild


def wild_entry(tree, path: str) -> Optional[tuple]:
    """(kind, spelling) of the first name on the way to `path`, on this side,
    that the fold key cannot judge, or None (round 22). Level by level from
    the top: the level's own name is wild and its directory holds any other
    entry, or its directory holds a wild entry that is not it. Either way
    what the path is, at and below that level, is unknown, so no twin
    spelling is composed: the caller fails the whole path closed."""
    entries, wild = tree_wild(tree)
    parts = path.split("/")
    for i in range(1, len(parts) + 1):
        prefix, parent = "/".join(parts[:i]), "/".join(parts[:i - 1])
        kind = wild_kind(parts[i - 1])
        if kind and entries.get(fold_key(parent), set()) - {prefix}:
            return kind, prefix
        for w in sorted(wild.get(fold_key(parent), ())):
            if w != prefix:
                return wild_kind(w.rpartition("/")[2]), w
    return None


def tree_fold_twins(tree) -> dict:
    """fold_key -> every tracked spelling (file, directory or link) on this
    side that has it, kept only for keys more than one spelling shares."""
    if getattr(tree, "_twins", None) is None:
        by = {}
        for p in set(tree.paths()) | tree_dirs(tree):
            by.setdefault(fold_key(p), set()).add(p)
        twins = {k: v for k, v in by.items() if len(v) > 1}
        try:
            tree._twins = twins
        except AttributeError:
            return twins
    return tree._twins


def fold_twins_of(path: str, trees: dict) -> list:
    """Every OTHER spelling, on either side, that a case-folding or
    normalising filesystem makes the same entry as `path` (round 15's
    fold_key), as (spelling, twinned prefix) pairs. A twin at ANY ancestor
    counts, not only a whole-path twin (round 17): with `config/` and
    `Config/` both tracked, `Config/app.yaml` IS `config/app.yaml` there,
    though no tracked file is spelled so. Each twinned prefix spelling is
    put in front of the rest of the path; the prefix is None for a
    whole-path twin. A name the fold key cannot judge composes no twin: it
    is `wild_entry`'s, and fails the path closed (round 22). The ONE place
    membership-by-exact-spelling consults twins."""
    parts = path.split("/")
    out = {}
    for i in range(len(parts), 0, -1):   # the whole path first: its reason wins
        prefix, rest = "/".join(parts[:i]), parts[i:]
        for side in ("base", "head"):
            for t in tree_fold_twins(trees[side]).get(fold_key(prefix), ()):
                if t != prefix:
                    out.setdefault("/".join([t] + rest), prefix if rest else None)
    out.pop(path, None)
    return sorted(out.items())


def twin_lang(tree, path: str) -> Optional[str]:
    """The language arm a tracked twin would be dispatched to, by its own
    extension, or by its own shebang when it has none."""
    ext = posixpath.splitext(path)[1].lower()
    if ext in EXT_LANG:
        return EXT_LANG[ext]
    if ext == "":
        text = tree.read(path)
        return shebang_lang(text.encode("utf-8", "surrogateescape")[:256]) if text else None
    return None


def apply_fold_twins(changes: list, cp: "CodePaths", trees: dict) -> None:
    """Round 16's F1, by class: every decision made by EXACT spelling -- the
    code_paths oracle, the language-arm dispatch by extension or shebang --
    is wrong for a file with a tracked fold twin, because on NTFS / APFS the
    twin IS this file. A non_executable file whose twin would be inside
    code_paths (or the oracle cannot say), or would be dispatched to a
    language arm other than this file's, is unknown (`fold_twin:<twin>`).
    A file with a name the fold key cannot judge on its way, at any level
    (`wild_entry`), is unknown outright (round 22): `unassigned_codepoint:`
    or `short_name:` and the name."""
    for c in changes:
        if c.verdict != NON_EXECUTABLE:
            continue
        for twin, prefix in fold_twins_of(c.path, trees):
            inside = cp.classify(twin)[0]
            lang = next((l for l in (twin_lang(trees[s], twin) for s in ("head", "base")) if l), None)
            if inside is not False or (lang is not None and lang != c.lang):
                c.verdict, c.reason = UNKNOWN, ("fold_twin_dir:%s:%s" % (prefix, twin)) if prefix else "fold_twin:" + twin
                break
        if c.verdict == NON_EXECUTABLE:
            hit = next((h for h in (wild_entry(trees[s], c.path) for s in ("head", "base")) if h), None)
            if hit:
                c.verdict, c.reason = UNKNOWN, "%s:%s" % hit


def tree_dirs(tree) -> set:
    """Every directory that holds a tracked path on this side."""
    if getattr(tree, "_dirs", None) is None:
        dirs = set()
        for p in tree.paths():
            d = posixpath.dirname(p)
            while d and d not in dirs:
                dirs.add(d)
                d = posixpath.dirname(d)
        try:
            tree._dirs = dirs
        except AttributeError:
            return dirs
    return tree._dirs


def disk_walk_to_root(real_root: str, cur: str, comps: list) -> tuple:
    """Walk `comps` on DISK from `cur`, one component at a time. ("ok",
    rest) the moment the walk stands on the root (`rest` is left for the
    tree to walk); ("above_root", "") otherwise -- never "it leaves the
    repository": where a path outside the checkout leads is a fact about
    THIS machine, not about the commit (round 13). A disk link that drops
    the walk BELOW the root is ("untracked", ""), since the working
    checkout's links may not be this side's."""
    rest = list(comps)
    while cur != real_root:
        if not rest:
            return "above_root", ""
        x = rest.pop(0)
        if x in ("", "."):
            continue
        cur = os.path.dirname(cur) if x == ".." else os.path.realpath(os.path.join(cur, x))
        if cur.startswith(real_root + os.sep):
            return "untracked", ""
    return "ok", "/".join(rest)


def resolve_in_tree(tree, start_dir: str, rel: str) -> tuple:
    """Resolve `rel` from the repository directory `start_dir` the way the
    FILESYSTEM does, against this side's tracked entries (round 12's F1).
    `..` is applied on disk AFTER symlinks are followed, so a string
    normpath is wrong: `sub/../data.txt` with `sub -> ../other/deep` reads
    `other/data.txt`. Returns (status, path):
      ok          -- `path` is the repository path reached, crossing no link;
      symlink     -- the walk reaches a tracked symlink (`path` names it);
                     what the filesystem reads from there is not followed;
      fold_twin   -- a component's path has another tracked spelling that a
                     case-folding or normalising filesystem makes the same
                     entry (round 15's F2), or the start directory or a
                     component has a name the fold key cannot judge on its
                     way (`wild_entry`, round 22); which one is read is not
                     decided;
      untracked   -- `..` out of a directory this side does not track (it
                     may be an untracked link, or not exist);
      above_root  -- the walk climbs above the repository root. Where it
                     goes from there depends on the name the repository is
                     checked out under, which no commit records: a default
                     `git clone` of `widget` resolves `../../widget/x` back
                     into itself, a worktree under another name does not
                     (round 13's F1/F2). It is never read as "leaves the
                     repository"; there is no "outside" verdict."""
    links = tree_symlinks(tree)
    dirs = tree_dirs(tree)
    twins = tree_fold_twins(tree)
    parts = [c for c in start_dir.split("/") if c]
    start_wild = wild_entry(tree, "/".join(parts)) if parts else None
    if start_wild:
        # The walk starts at or under a name the fold key cannot judge
        # (round 22), so the directory it reads from, and every directory
        # `..` re-enters, is unknown.
        return "fold_twin", start_wild[1]
    for c in rel.replace("\\", "/").split("/"):
        if c in ("", "."):
            continue
        if c == "..":
            if not parts:
                return "above_root", ""
            here = "/".join(parts)
            if here not in dirs:
                return "untracked", here
            parts.pop()
            continue
        parts.append(c)
        prefix = "/".join(parts)
        if prefix in links:
            return "symlink", prefix
        if fold_key(prefix) in twins:
            # Another tracked spelling folds to this one (`gen/gen.txt` and
            # `GEN/gen.txt`, an NFC and an NFD name): on NTFS / APFS they are
            # one entry, so which of them -- or which link -- the walk reads
            # here is not decided by the spelling (round 15's F2).
            return "fold_twin", prefix
        if wild_entry(tree, prefix):
            # A name the fold key cannot judge is on the way to this entry,
            # or beside it: one with an unassigned code point, or an 8.3 short
            # name (rounds 21 and 22). What the walk reads is not decided.
            return "fold_twin", prefix
    return "ok", "/".join(parts)


def symlink_aliases(tree, path: str) -> list:
    """The tracked links on this side through which `path` may ALSO be
    read: a link whose target resolves to it, a link to a directory holding
    it (or to the root), and -- fail-closed -- EVERY link whose target
    cannot be followed exactly inside the commit: through another link,
    through an untracked directory, unreadable, climbing above the root by
    `..` (round 13: whether it lands back in the checkout depends on the
    name the checkout has, so no checkout can prove it does not), or
    absolute and not resolving onto this root here. No target is ever read
    as leaving the repository; an absolute target that the disk walk lands
    on this root is followed into the tree, the only case resolved
    precisely."""
    hits = []
    root = tree.root
    real_root = os.path.realpath(root)
    for link, target in sorted(tree_symlinks(tree).items()):
        kind = path_spelling_kind(target)
        if "\0" in target or kind == "foreign":
            # Unreadable, or spelled for a resolver this walk is not (round
            # 14's F2: `\\rooted`, `\\\\UNC\\..`, `C:..`).
            hits.append(link)
            continue
        if kind == "posix_absolute":
            st, rest = disk_walk_to_root(real_root, os.sep, target.split("/"))
            if st != "ok":
                hits.append(link)  # absolute, and not landing on this root here
                continue
            st, resolved = resolve_in_tree(tree, "", rest)
        else:
            st, resolved = resolve_in_tree(tree, posixpath.dirname(link), target)
        if st != "ok":
            hits.append(link)  # not followed exactly: fail closed
            continue
        if resolved == "" or path == resolved or path.startswith(resolved + "/"):
            hits.append(link)
        elif resolved not in tree.paths() and resolved not in tree_dirs(tree):
            # Reaches nothing this side tracks by that exact spelling -- the
            # include arm's `unresolved_path`, here (round 14's F1). On a
            # case-folding or trailing-dot-stripping filesystem (NTFS, APFS)
            # `../GEN/gen.txt` and `../gen/gen.txt.` ARE gen/gen.txt.
            hits.append(link)
    return hits


def apply_symlink_aliases(changes: list, trees: dict) -> None:
    """Round 11's F4, by class: a non_executable file that a tracked symlink
    names (directly or through a linked directory) on EITHER side is read
    under the link's path as well -- by `include_str!("link")`, `mod` via a
    linked `.rs`, a doctest collector following the link -- and that reader
    was judged against the LINK, not this file. It is unknown. Each side's
    links are resolved against that side's own tree, so a link retargeted
    between base and head names its base target AND its head target (round
    12's F4)."""
    for c in changes:
        if c.verdict != NON_EXECUTABLE:
            continue
        hits = []
        for side in ("base", "head"):  # both sides: a retargeted link names two files
            hits.extend(h for h in symlink_aliases(trees[side], c.path) if h not in hits)
        if hits:
            c.verdict, c.reason = UNKNOWN, "aliased_by_symlink:" + hits[0]


def rust_doctest_scan(rs: str, doc_texts: list, scan: "IncludeScan", what: str, depth: int) -> None:
    """Rule B for DOCTESTS (round 9's F2/F3): every doctest in `doc_texts`
    is Rust code, so it gets the same TOKEN scan as a repository file --
    rules 1a-1d, the reader names, `name_outside_an_invocation` -- not only
    the raw name regexes. A finding inside one is opaque whatever it is (a
    doctest's include resolves against no file here)."""
    for doc in doc_texts:
        for src in doctest_sources(doc):
            if depth >= DOCTEST_MAX_DEPTH:
                scan.opaque.append((rs, what + ":doctest:nesting_too_deep"))
                return
            sub = IncludeScan()
            rust_include_scan_text(_NoTree(), rs, src, sub, depth + 1)
            scan.opaque.extend((f, "%s:doctest:%s" % (what, why)) for f, why in sub.opaque)
            scan.readers.extend(sub.readers)
            scan.opaque.extend((rs, "%s:doctest:include:%s" % (what, p[0])) for p in sub.plain)


@dataclass
class IncludeScan:
    plain: list = field(default_factory=list)     # (resolved path, is_doc, from_file, join)
    opaque: list = field(default_factory=list)    # (from_file, why)
    readers: list = field(default_factory=list)   # (from_file, reader)
    raw_scanned: set = field(default_factory=set)  # (target, label) already raw-scanned


def rust_include_scan_text(tree: "RustTree", rs: str, text: str, scan: IncludeScan, depth: int = 0) -> list:
    """Scan one Rust source text into `scan`; returns the repository paths it
    `include!`s as Rust source (to be scanned in turn). `depth` counts how
    many doctests deep this text sits (0: a repository file)."""
    toks, _docs = _rs_flat_tokens(text)
    words = [t for t, _ in toks]
    n = len(words)
    # Matching brackets, and what each opened region is.
    match, stack = {}, []
    for k, w in enumerate(words):
        if w in _RS_OPENERS:
            stack.append(k)
        elif w in (")", "]", "}") and stack:
            match[stack.pop()] = k
    region_of, region_macro = {}, {}
    for k, w in enumerate(words):
        if w == "#":
            j = k + 1 + (1 if k + 1 < n and words[k + 1] == "!" else 0)
            if j < n and words[j] == "[":
                region_of[j] = "attr"
        elif w == "macro_rules" and k + 3 < n and words[k + 1] == "!" and words[k + 3] in _RS_OPENERS:
            region_of[k + 3] = "macro_body"
        elif w == "macro" and k + 2 < n and _rs_is_ident(words[k + 1][:1]) and words[k + 2] in _RS_OPENERS:
            region_of[k + 2] = "macro_body"
            after = match.get(k + 2)
            if after is not None and after + 1 < n and words[after + 1] == "{":
                region_of[after + 1] = "macro_body"
        elif w == "!" and k + 1 < n and words[k + 1] in _RS_OPENERS and k > 0 \
                and _rs_is_ident(words[k - 1][:1]) and (k + 1) not in region_of:
            region_of[k + 1] = "macro_args"
            qualified = k >= 2 and words[k - 2] == ":"
            region_macro[k + 1] = None if qualified else words[k - 1]
    local_macros = {words[k + 2] for k in range(n - 2) if words[k] == "macro_rules" and words[k + 1] == "!"}
    # `macro assert_eq(..) {..}` (decl_macro) shadows a prelude macro too.
    local_macros |= {words[k + 1] for k in range(n - 1) if words[k] == "macro" and _rs_is_ident(words[k + 1][:1])}
    # Round 10's B3: a std macro name is trusted only while nothing can have
    # rebound it. A `use` naming it (`use x::documented as assert_eq;`) shadows
    # the prelude macro; a glob `use` or a `#[macro_use]` may bring a
    # same-named macro in under no visible name at all, so either revokes
    # every name; and another file's `macro_rules!` / `use` / `#[macro_use]`
    # reaches this one through textual scope or a re-export.
    revoke_all = False
    for k, w in enumerate(words):
        if w == "use" and (k == 0 or words[k - 1] not in (".", ":")):
            j = k + 1
            while j < n and words[j] != ";":
                if words[j] in _RS_EXPR_MACROS:
                    local_macros.add(words[j])
                elif words[j] == "*":
                    revoke_all = True
                j += 1
        elif w == "macro_use":
            revoke_all = True
    tree_shadows = []

    def rebound(m) -> bool:
        if m in local_macros or revoke_all:
            return True
        if not tree_shadows:
            tree_shadows.append(tree.std_macro_shadows())
        names, everything = tree_shadows[0]
        return everything or m in names

    def std_trusted(m) -> bool:
        if m is None or m not in _RS_EXPR_MACROS:
            return False
        return not rebound(m)

    def metavar_ends_at(j: int) -> bool:
        """Does a metavariable (`$name`) or a repetition (`$( .. )*`, with or
        without a separator) end at token j? Either may expand to a `#`."""
        if j < 1:
            return False
        if words[j - 1] == "$" and _rs_is_ident(words[j][:1]):
            return True
        return words[j] in ("*", "+", "?") and (words[j - 1] == ")" or (j >= 2 and words[j - 2] == ")"))

    regions, open_at = [], []
    nested = []
    doc_strings = []
    for k, w in enumerate(words):
        if w == "#" and not (k + 1 < n and (words[k + 1] == "[" or words[k + 1] == "$"
                                            or (words[k + 1] == "!" and k + 2 < n and words[k + 2] == "["))) \
                and not (k > 0 and words[k - 1] == "r" and toks[k - 1][1] + 1 == toks[k][1]) \
                and not (k == 0 and toks[k][1] == 0 and text.startswith("#!")):
            # Rule 1c: a `#` that does not open an attribute here (a raw
            # identifier and a leading shebang aside) only means something
            # as a token a macro will place: handed to a macro as a `tt`
            # (`mk!(#, doc, ..)`), it can be put in front of ANY bracket
            # group, and so open an attribute this scan never sees spelled.
            scan.opaque.append((rs, "stray_pound_token"))
        if "macro_body" in regions and w == "$" \
                and (metavar_ends_at(k - 1) or (k >= 2 and words[k - 1] == "!" and metavar_ends_at(k - 2))):
            # Rule 1d: a metavariable followed -- directly or across one `!`
            # -- by another metavariable (`$h $t`, `$p ! $q`) may be a `#`
            # and a bracket group, i.e. an attribute (outer or inner)
            # assembled in expansion. The `!` arm is round 9's F1: `#[]`
            # handed in as `$p:tt []` keeps the `#` from being stray, and
            # `$p ! $q` then builds `#![..]`. A literal bracket group after
            # a metavariable (`$h [..]`, `$h ! [..]`) is rule 1b's.
            scan.opaque.append((rs, "macro_adjacent_metavariables"))
        if w in _RS_OPENERS:
            open_at.append(k)
            region = region_of.get(k, "plain")
            if w == "[" and "macro_body" in regions and region != "macro_body" \
                    and (metavar_ends_at(k - 1) or (k >= 1 and words[k - 1] == "!" and metavar_ends_at(k - 2))):
                # Rule 1b, extended: a bracket group after a metavariable
                # (`$h [..]`, `$h ! [..]`) is an attribute when `$h` is `#`.
                region = "attr"
            regions.append(region)
        elif w in (")", "]", "}") and open_at and match.get(open_at[-1]) == k:
            open_at.pop()
            regions.pop()
        prev = words[k - 1] if k > 0 else ""
        nxt = words[k + 1] if k + 1 < n else ""
        if _rs_is_str_token(w) and ("attr" in regions or "macro_body" in regions or any(
                not std_trusted(region_macro.get(o))
                for o, r in zip(open_at, regions) if r == "macro_args")):
            # A literal that is, or may become, doc text: inside an attribute
            # (`#[doc = ".."]`), a macro body, or the arguments of a macro
            # that may hand it to `#[doc = $x]`. Its doctests are code.
            doc_strings.append(_rs_str_value(w))
        if w == "doc" and nxt == "=" and (k + 2 >= n or words[k + 2] != "=") and not _doc_value_plain(words, k, match):
            # Rule 1a: `doc = <value>` ANYWHERE in code -- an attribute, a
            # macro's arguments, a binding -- whose value is not a plain
            # literal or a plain include_str! is doc text this script cannot
            # read. Judged on code tokens, so no comment can hide it.
            scan.opaque.append((rs, "token:doc_attribute_non_literal"))
        if "macro_body" in regions:
            # Rule 1b: an attribute a macro_rules! body builds out of a
            # metavariable (`#[$m]`, `#[doc = $x]`, `#[cfg_attr(.., $m)]`, or
            # `#$m` with a bracketed tt) may be ANY meta, doc included.
            if w == "$" and "attr" in regions:
                scan.opaque.append((rs, "macro_attribute_metavariable"))
            elif w == "#":
                j = k + 1 + (1 if nxt == "!" else 0)
                if j < n and words[j] == "$":
                    scan.opaque.append((rs, "macro_attribute_metavariable"))
        if w in OTHER_READER_IDENTS or (w in OTHER_READER_MACROS and nxt == "!"):
            scan.readers.append((rs, w))
            continue
        if "attr" in regions:
            for word, arg in OTHER_READER_ATTR_ARGS:
                if w != word:
                    continue
                if arg is None and nxt == "=" and (k + 2 >= n or words[k + 2] != "="):
                    scan.readers.append((rs, "#[%s = ..]" % word))
                elif arg is not None and nxt == "(" and arg in words[k + 2:match.get(k + 1, k + 1)]:
                    scan.readers.append((rs, "#[%s(%s = ..)]" % (word, arg)))
        attr_open = next((o for o, r in zip(reversed(open_at), reversed(regions)) if r == "attr"), None)
        attr_name = words[attr_open + 1] if attr_open is not None and attr_open + 1 < n else ""
        if w == "path" and nxt == "=" and k + 2 < n and (prev == "[" and attr_open == k - 1 or attr_name == "cfg_attr"):
            # `#[path = "x"] mod m;` compiles x as Rust source: a .rs target
            # has a language arm of its own, anything else is refused.
            target = _rs_plain_literal(words[k + 2])
            if target is None or not target.endswith(".rs"):
                scan.opaque.append((rs, "path_attribute"))
            continue
        if w not in INCLUDE_NAMES:
            continue
        in_body = "macro_body" in regions
        in_macro = in_body or "macro_args" in regions
        if nxt != "!":
            path_prefix = nxt == ":" and k + 2 < n and words[k + 2] == ":"
            field_like = nxt == ":" and not path_prefix
            if not in_macro and (prev in _RS_BINDING_WORDS or path_prefix or field_like):
                continue
            scan.opaque.append((rs, "name_outside_an_invocation:" + w))
            continue
        if in_body:
            scan.opaque.append((rs, "in_a_macro_body:" + w))
            continue
        if prev == ":" and k >= 2 and words[k - 2] == ":":
            q = words[k - 3] if k >= 3 else ""
            lead = k - 4
            rooted = lead >= 1 and words[lead] == ":" and words[lead - 1] == ":"
            if q not in ("std", "core") or (rooted and lead >= 2 and _rs_is_ident(words[lead - 2][:1])) \
                    or (not rooted and lead >= 0 and words[lead] == ":"):
                scan.opaque.append((rs, "qualified_path:" + w))
                continue
        opener = k + 2
        close = match.get(opener) if opener < n and words[opener] in _RS_OPENERS else None
        if close is None:
            scan.opaque.append((rs, "no_delimiter:" + w))
            continue
        args = words[opener + 1:close]
        if [a for a in args[:8] if a != ","] == ["concat", "!", "(", "env", "!", "(", '"OUT_DIR"', ")"] \
                and len(args) in (11, 12) and args[8] == "," and re.fullmatch(r'"[^"\\]*"', args[9]) and args[10:] in ([")"], [",", ")"], [")", ","]):
            # A file under OUT_DIR, generated into the build directory and
            # never a repository file; a build script that copies one there
            # is the R2 residual.
            continue
        lit = _rs_plain_literal(args[0]) if args and (len(args) == 1 or args[1:] == [","]) else None
        if lit is None:
            scan.opaque.append((rs, "non_literal_argument:" + w))
            continue
        enclosing = [region_macro.get(o) for o, r in zip(open_at, regions) if r == "macro_args"]
        if any(not std_trusted(m) for m in enclosing):
            # Handed to a macro that may turn it into doc text (or into
            # anything): what reaches rustdoc is unknown.
            scan.opaque.append((rs, "include_in_macro_arguments:" + w))
            continue
        st, resolved = resolve_in_tree(tree, posixpath.dirname(rs), lit)
        if st == "symlink":
            # A tracked symbolic link on the way, or at the end: the compiler
            # reads what it points at, a file this scan does not name (round
            # 11's F4; round 12's F1 for `sub/../x` through a linked `sub`).
            scan.opaque.append((rs, "include_through_symlink:" + lit))
            continue
        if st != "ok" or resolved not in tree.paths():
            # Not a tracked file by this exact spelling: through a symlinked
            # directory, case-folded by the filesystem, outside the
            # repository, or generated. The file actually read is unknown.
            scan.opaque.append((rs, "unresolved_path:" + lit))
            continue
        is_doc = k >= 3 and prev == "=" and words[k - 2] == "doc" and words[k - 3] == "["
        join = None
        if is_doc:
            join = doc_include_join(tree, rs, text, rust_segments(text), toks[k][1])
        scan.plain.append((resolved, is_doc, rs, join))
        if w == "include":
            nested.append(resolved)
        else:
            # EVERY plain include_str! / include_bytes! target is scanned raw,
            # wherever the include sits: one inside an attribute is doc text
            # outright, and one anywhere else may reach an attribute through a
            # macro, so its whole text is judged as doc text either way.
            label = ("attribute_included_text:" if "attr" in regions else "included_text:") + resolved
            if (resolved, label) not in scan.raw_scanned:
                scan.raw_scanned.add((resolved, label))
                target = tree.read(resolved)
                if target is None:
                    scan.opaque.append((rs, "include_unreadable:" + resolved))
                else:
                    # A .rs target included OUTSIDE an attribute (and, by the
                    # check above, outside any macro but a std expression
                    # one) is a &str constant that never becomes doc text,
                    # and its CODE tokens are judged by its own token scan
                    # (git grep finds every tracked .rs naming an include):
                    # only its comments and literals are read raw here. Any
                    # other target, and any target of an attribute, is read
                    # whole -- every byte of it may be doc text.
                    excused = set()
                    if "attr" not in regions and resolved.endswith(".rs"):
                        excused = {s for _, s in _rs_flat_tokens(target)[0]}
                        doc_texts = _rs_doc_comment_texts(target)
                    else:
                        doc_texts = [target]
                    rust_raw_text_scan(rs, target, excused, scan, label)
                    rust_doctest_scan(rs, doc_texts, scan, label, depth)
    # Every CODE token was judged above; the raw-text scan judges what lies
    # outside them -- comments, doc comments, and the inside of string
    # literals (a `#[doc = ".."]` doctest) -- so a code token's start offset
    # is the one place it passes a name over.
    rust_raw_text_scan(rs, text, {s for _, s in toks}, scan, "raw_text")
    rust_doctest_scan(rs, _rs_doc_comment_texts(text) + doc_strings, scan, "raw_text", depth)
    return nested


def rust_include_scan(tree: "RustTree") -> IncludeScan:
    """Every include-family and other-reader occurrence in one side's tree:
    each .rs file naming one, and every file those `include!` as source."""
    scan = IncludeScan()
    words = list(INCLUDE_NAMES) + sorted(OTHER_READER_IDENTS | OTHER_READER_MACROS) \
        + sorted({w for w, _ in OTHER_READER_ATTR_ARGS}) + ["path"]
    # `doc` finds a non-literal `doc = ..`; a backslash, a name only an
    # escape spells (the raw-text scan decodes `\x..` / `\u{..}`).
    words += ["doc", "\\"]
    # `$`: a macro_rules! body that builds an attribute out of a
    # metavariable (rule 1b) may live in a file naming nothing else.
    words += ["$"]
    queue = sorted(tree.grep_rs(words))
    tree.prefetch(queue)
    seen = set()
    while queue:
        rs = queue.pop(0)
        if rs in seen:
            continue
        seen.add(rs)
        text = tree.read(rs)
        if text is None:
            if rs.endswith(".rs"):
                scan.opaque.append((rs, "unreadable"))
            continue
        queue.extend(rust_include_scan_text(tree, rs, text, scan))
    return scan


def apply_rust_includes(root: str, base_sha: str, head_sha: Optional[str], changes: list,
                        trees: Optional[dict] = None) -> None:
    """Re-judge a non_executable file that Rust source may compile in:
    executable when a plain include names it (a doc include follows the joined
    doc rules), and unknown when ANY include is opaque or any other
    compile-time reader is present, since either may name it. A CODE file is
    a candidate too: `include_str!("tool.sh")` embeds its comments, so a
    comment-only edit to it changes the program."""
    candidates = [c for c in changes if c.verdict == NON_EXECUTABLE]
    if not candidates:
        return
    trees = trees or {"base": RustTree(root, base_sha), "head": RustTree(root, head_sha)}
    scans = [rust_include_scan(trees["base"]), rust_include_scan(trees["head"])]
    includes = [inc for s in scans for inc in s.plain]
    # A doctest finding sorts after the file's other findings, so the
    # reason names the plainest shape that fired.
    opaque = sorted({o for s in scans for o in s.opaque}, key=lambda o: (o[0], ":doctest:" in o[1], o[1]))
    readers = sorted({r for s in scans for r in s.readers})
    for c in candidates:
        hits = [inc for inc in includes if inc[0] == c.path]
        if hits:
            rs = hits[0][2]
            if c.status != "M" or c.base is None or c.head is None or not all(h[1] for h in hits):
                c.verdict, c.reason = EXECUTABLE, "included_by_rust:" + rs
                continue
            text_b, text_h = c.base.decode("utf-8", "replace"), c.head.decode("utf-8", "replace")
            joined = next((h for h in hits if h[3]), None)
            if code_blocks(text_b) != code_blocks(text_h):
                c.verdict, c.reason = EXECUTABLE, "rust_doc_include_code_block:" + rs
                continue
            if doc_layout_ambiguous(text_b) or doc_layout_ambiguous(text_h):
                c.verdict, c.reason = EXECUTABLE, "rust_doc_include_block_context:" + rs
                continue
            if joined is not None:
                c.verdict, c.reason = EXECUTABLE, "rust_doc_include_joined:%s:%s" % (joined[2], joined[3])
                continue
            c.reason = "outside_code_paths:rust_doc_include_prose_only"
        if opaque:
            c.verdict, c.reason = UNKNOWN, "included_by_rust_opaque:%s:%s" % opaque[0]
        elif readers:
            c.verdict, c.reason = UNKNOWN, "compile_time_reader:%s:%s" % (readers[0][1], readers[0][0])


# ---------------------------------------------------------------------------
# driver
# ---------------------------------------------------------------------------

def parse_args(argv: list) -> argparse.Namespace:
    class Parser(argparse.ArgumentParser):
        def error(self, message):
            raise UsageError(message)

    p = Parser(prog="exec-equivalence.py", add_help=True,
               description="Per changed file: non_executable | executable | unknown.")
    p.add_argument("--repo-root", required=True)
    p.add_argument("--base", required=True)
    p.add_argument("--head")
    p.add_argument("--code-paths", dest="code_paths")
    p.add_argument("--json", action="store_true")
    try:
        return p.parse_args(argv)
    except SystemExit as exc:
        # --help exits through here. Its 0 would read as "every file non_executable".
        raise UsageError("help printed (the parser exited %s)" % exc.code)


def run(argv: list) -> tuple:
    args = parse_args(argv)
    if not git_scope_strip():
        raise UsageError("could not clear an inherited GIT_DIR / GIT_WORK_TREE; git answers would be about another repository")
    for name in ATTR_ENV:   # round 28's F5: attributes are read from the tree they belong to
        os.environ.pop(name, None)
    if not os.path.isdir(args.repo_root):
        raise UsageError("--repo-root %r is not a directory" % args.repo_root)
    # Round 32: the root git prints, less only its LF -- a work tree may end in a
    # space, and a stripped path can name a sibling repository.
    root = _git_path_line(git(args.repo_root, "rev-parse", "--show-toplevel"))
    if not root:
        raise UsageError("%r is not inside a git work tree" % args.repo_root)
    base_sha = resolve_commit(root, args.base)
    head_sha = resolve_commit(root, args.head) if args.head else None

    arg_paths = None
    if args.code_paths is not None:
        try:
            arg_paths = json.loads(args.code_paths)
        except ValueError as exc:
            raise UsageError("--code-paths is not JSON: %s" % exc) from exc
        if not isinstance(arg_paths, list) or not all(isinstance(g, str) for g in arg_paths):
            raise UsageError("--code-paths must be a JSON array of glob strings")

    changes = list_changes(root, base_sha, head_sha)
    attributed = checkout_attribute_reasons(root, changes, base_sha, head_sha)
    for c in changes:
        if c.path in attributed:   # attributes change what a checkout holds: no stored form is the file
            c.verdict, c.reason = UNKNOWN, attributed[c.path]
    todo = [c for c in changes if c.reason == "not evaluated"]
    blobs = read_blobs(root, [c.old_sha for c in todo if wants_base(c)]
                       + [c.new_sha for c in todo if wants_head(c) and c.new_sha != NULL_SHA])
    off_disk = [c for c in todo if wants_head(c) and c.new_sha == NULL_SHA
                and (c.untracked or c.on_disk or c.status == "T")]
    cleaned = clean_blobs(root, [c.path for c in off_disk if c.new_mode != MODE_SYMLINK])
    # Every head side is read WHOLE, as the commit would carry it (rounds 26
    # and 27): a blob where git names one (a commit's, or the index's where
    # the working tree matches it); otherwise the working tree, as git would
    # store it.
    for c in todo:
        if wants_base(c):
            c.base = blobs[c.old_sha]
        if wants_head(c):
            if c.new_sha != NULL_SHA:
                c.head = blobs[c.new_sha]
            elif c.untracked or c.on_disk or c.status == "T":
                c.head = read_link(root, c.path) if c.new_mode == MODE_SYMLINK else cleaned.get(c.path)
            elif c.status in ("M", "A"):   # tracked, staged or not, its mode changed or not
                c.head = reconstruct_worktree(root, base_sha, c.path, c.base or b"")
            if c.head is None:
                c.verdict, c.reason = UNKNOWN, "worktree_reconstruction_failed"
        c.lang = detect_lang(c)

    coord_url, fixture_mode = coord_base_url()
    ts = TreeSitter()
    cp = CodePaths()
    cred = Credential()

    pending = [c for c in todo if c.reason != "worktree_reconstruction_failed"]
    # A language file that is not a plain content edit is executable outright.
    for c in pending:
        if c.lang and (c.status != "M" or is_special(c) or c.old_mode != c.new_mode):
            c.arm, c.verdict, c.reason = c.lang, EXECUTABLE, structural_reason(c)
    pending = [c for c in pending if c.reason == "not evaluated"]

    need_paths = any(c.lang is None for c in pending)
    need_python = any(c.lang == "python" for c in pending)
    need_ts = any(c.lang in TREE_SITTER_LANGS for c in pending)

    slug = origin_slug(root) if (need_paths or need_python) else None
    if need_paths and arg_paths is not None:
        cp = CodePaths(source="arg")
        cp.load(arg_paths)
    url_ok = coord_url_ok(coord_url)
    needs_owner = need_paths and arg_paths is None
    if needs_owner or need_python:
        if url_ok:
            cred = stage_credential(coord_url, slug, needs_owner)
        else:
            cred = Credential(state="none", note="coord url %s is neither https nor loopback; no credential is sent to it" % coord_url)
    if needs_owner:
        cp = read_code_paths_from_coord(coord_url, slug, cred)
    if need_ts:
        ts.load()

    rust_trees = {"base": RustTree(root, base_sha), "head": RustTree(root, head_sha)}
    for c in pending:
        if c.lang == "python":
            python_arm(c, coord_url, cred)
        elif c.lang in TREE_SITTER_LANGS:
            tree_sitter_arm(c, ts, rust_trees)
        else:
            code_paths_arm(c, cp)
    apply_rust_includes(root, base_sha, head_sha, changes, rust_trees)
    apply_symlink_aliases(changes, rust_trees)
    apply_fold_twins(changes, cp, rust_trees)
    changes = keep_worse(changes)

    counts = {v: sum(1 for c in changes if c.verdict == v) for v in (NON_EXECUTABLE, EXECUTABLE, UNKNOWN)}
    if counts[EXECUTABLE]:
        overall, code = EXECUTABLE, EXIT_EXECUTABLE
    elif counts[UNKNOWN]:
        overall, code = UNKNOWN, EXIT_UNKNOWN
    else:
        overall, code = NON_EXECUTABLE, EXIT_OK
    report = {
        "verdict": overall,
        "exit_code": code,
        "repo_root": root,
        "base": base_sha,
        "head": head_sha or "WORKTREE",
        "files": [{"path": c.path, "status": c.status, "verdict": c.verdict, "reason": c.reason,
                   "arm": c.arm, "language": c.lang} for c in changes],
        "counts": counts,
        "code_paths": {"source": cp.source, "state": cp.state, "globs": cp.globs,
                       "dropped": cp.dropped, "complete": cp.complete, "note": cp.note},
        "coord": {"url": coord_url, "fixture_mode": fixture_mode, "url_ok": url_ok,
                  "credential": {"state": cred.state, "owner_state": cred.owner_state,
                                 "tenant": cred.tenant, "source": cred.source, "note": cred.note}},
        "tree_sitter": {"pack": TREE_SITTER_PACK, "pin": TREE_SITTER_PACK_PIN, "state": ts.state,
                        "found": ts.found, "core": ts.core, "reason": ts.reason},
        "unicode": unicodedata.unidata_version,
    }
    return report, code, args.json


def one_field(text: str) -> str:
    """A path or reason as ONE tab-separated field: a tab, CR or LF inside it
    (a git path may hold any of them; a coord error text may too) is escaped,
    so a consumer splitting on tabs and lines always reads three fields.
    --json carries the exact bytes."""
    return text.replace("\\", "\\\\").replace("\t", "\\t").replace("\r", "\\r").replace("\n", "\\n")


def render(report: dict) -> str:
    lines = []
    for f in report["files"]:
        lines.append("%s\t%s\t%s" % (f["verdict"], one_field(f["path"]), one_field(f["reason"])))
    c = report["counts"]
    lines.append("EXEC_EQUIVALENCE verdict=%s exit=%d files=%d non_executable=%d executable=%d unknown=%d base=%s head=%s"
                 % (report["verdict"], report["exit_code"], len(report["files"]), c[NON_EXECUTABLE],
                    c[EXECUTABLE], c[UNKNOWN], report["base"], report["head"]))
    return "\n".join(lines) + "\n"


def provenance(report: dict) -> str:
    cred = report["coord"]["credential"]
    cp = report["code_paths"]
    ts = report["tree_sitter"]
    return ("exec-equivalence: coord=%s%s credential=%s%s%s code_paths=%s/%s%s tree_sitter=%s%s unicode=%s\n" % (
        report["coord"]["url"], " (FIXTURE MODE)" if report["coord"]["fixture_mode"] else "",
        cred["state"], (" tenant=" + cred["tenant"]) if cred["tenant"] else "",
        (" (" + cred["note"] + ")") if cred["note"] and cred["state"] != "proven" else "",
        cp["source"], cp["state"], (" (" + cp["note"] + ")") if cp["note"] else "",
        ts["state"], (" " + ts["reason"]) if ts["reason"] else "", report["unicode"]))


def main(argv: list) -> int:
    try:
        report, code, as_json = run(argv)
    except UsageError as exc:
        print("exec-equivalence: %s -- no verdict" % exc, file=sys.stderr)
        return EXIT_USAGE
    except Exception as exc:  # an internal fault must never read as a verdict
        print("exec-equivalence: internal error %s: %s -- no verdict" % (type(exc).__name__, exc), file=sys.stderr)
        return EXIT_USAGE
    if as_json:
        sys.stdout.write(json.dumps(report, indent=2, sort_keys=True) + "\n")
    else:
        sys.stdout.write(render(report))
        sys.stderr.write(provenance(report))
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
