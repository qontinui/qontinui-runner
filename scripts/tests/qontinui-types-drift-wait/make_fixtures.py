# Fixture generator for scripts/tests/qontinui-types-drift-wait/run.sh. Writes fx/<case>/<api path> files
# (body, then the HTTP status on the last line) into the current directory.
import json, os, sys
E='repos_qontinui_qontinui-schemas_issues_events_per_page_100_page_'
def ev(m, label, at, actor='qontinui-merge-orchestrator[bot]', event='unlabeled', pr=True):
    d={'event':event,'actor':{'login':actor},'label':{'name':label},'created_at':at,'issue':{'number':m}}
    if pr: d['issue']['pull_request']={'url':'x'}
    return d
def w(d, name, body, status=200):
    os.makedirs(d, exist_ok=True)
    open(os.path.join(d,name),'w').write((json.dumps(body) if not isinstance(body,str) else body)+'\n'+str(status))
def rpr(d,n,state='closed',landed=True,merged=None,base='main',status=200):
    w(d,f'repos_qontinui_qontinui-runner_pulls_{n}',{'state':state,'base':{'ref':base},'merged_at':merged,'labels':[{'name':'coord:landed'}] if landed else []},status)
GEN=[{'filename':'ts/src/generated/ManagedBy.d.ts','status':'added'},
     {'filename':'ts/src/generated/ScheduledTask.d.ts','status':'modified'},
     {'filename':'src/qontinui_schemas/generated/__init__.py','status':'modified'},
     {'filename':'codegen/pending-registration.toml','status':'modified'}]
def spr(d,m,state='open',base='main',sha='a'*40,landed=False,files=None,cmp_status=200,cmp_state='ahead',mb='b'*40):
    w(d,f'repos_qontinui_qontinui-schemas_pulls_{m}',{'state':state,'base':{'ref':base},'head':{'sha':sha},'merged_at':None,'labels':[{'name':'coord:landed'}] if landed else []})
    # GitHub's compare of main...<head>: `status` is head relative to main, and a
    # head already on main (behind/identical) lists no files.
    cmp={'files':GEN if files is None else files}
    if mb is not None: cmp['merge_base_commit']={'sha':mb}
    if cmp_state is not None: cmp['status']=cmp_state
    w(d,f'repos_qontinui_qontinui-schemas_compare_main...{sha}',cmp,cmp_status)
L=lambda n: f'coord:downstream-of=qontinui-runner#{n}'
T='2026-11-05T08:15:55Z'
# ok: one candidate (owner-qualified form)
d='fx/ok'; w(d,E+'1',[ev(285,'coord:downstream-of=qontinui/qontinui-runner#2546',T)]); rpr(d,2546); spr(d,285)
d='fx/human'; w(d,E+'1',[ev(285,L(2546),T,actor='jspinak')]); rpr(d,2546); spr(d,285)
d='fx/notlanded'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546,state='open',landed=False); spr(d,285)
d='fx/closedunlanded'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546,landed=False); spr(d,285)
d='fx/merged'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546,landed=False,merged='2026-11-05T08:15:53Z'); spr(d,285)
d='fx/mclosed'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,state='closed')
d='fx/mbase'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,base='release')
d='fx/old'; w(d,E+'1',[ev(285,L(2546),'2026-11-01T08:00:00Z')]); rpr(d,2546); spr(d,285)
d='fx/two'; w(d,E+'1',[ev(285,L(2546),T),ev(290,L(2550),T)]); rpr(d,2546); rpr(d,2550); spr(d,285); spr(d,290,sha='b'*40)
d='fx/samemtwice'; w(d,E+'1',[ev(285,L(2546),'2026-11-05T09:00:00Z'),ev(285,L(2546),T),ev(285,L(2547),T)]); rpr(d,2546); rpr(d,2547); spr(d,285)
# api403 / runner500 / notarray: well-formed enough that ONLY the guard under test refuses them.
d='fx/api403'; w(d,E+'1',[ev(285,L(2546),T)],403); rpr(d,2546); spr(d,285)
d='fx/runner500'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546,status=500); spr(d,285)
d='fx/notpr'; w(d,E+'1',[ev(285,L(2546),T,pr=False)]); rpr(d,2546); spr(d,285)
d='fx/otherlabel'; w(d,E+'1',[ev(285,'coord:downstream-of=qontinui-runner#2546x',T),ev(286,'coord:upstream-of=qontinui-runner#2546',T),ev(287,'coord:downstream-of=ui-bridge#5',T)])
d='fx/labeled'; w(d,E+'1',[ev(285,L(2546),T,event='labeled')]); rpr(d,2546); spr(d,285)
d='fx/badsha'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,sha='zz')
# 3 full pages, all recent -> UNKNOWN
d='fx/full'
for p in (1,2,3): w(d,E+str(p),[ev(1,'x','2026-11-05T10:00:00Z',event='labeled')]*100)
# page1 full recent, page2 short -> reached
d='fx/page2'; w(d,E+'1',[ev(1,'x','2026-11-05T10:00:00Z',event='labeled')]*100); w(d,E+'2',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285)
d='fx/notarray'; w(d,E+'1',{'0':ev(285,L(2546),T)}); rpr(d,2546); spr(d,285)
d='fx/empty'; w(d,E+'1',[])
d='fx/runneropenlanded'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546,state='open'); spr(d,285)
d='fx/runnerbase'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546,base='release'); spr(d,285)
d='fx/mlandedlabel'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,landed=True)
d='fx/codetouch'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=GEN+[{'filename':'scripts/compile_typescript.mjs','status':'modified'}])
d='fx/rusttouch'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=GEN+[{'filename':'rust/src/scheduler.rs','status':'modified'}])
d='fx/cmp300'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=[{'filename':f'ts/src/generated/T{i}.d.ts','status':'added'} for i in range(300)])
d='fx/dotpath'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=GEN+[{'filename':'ts/src/generated/../../../scripts/compile_typescript.mjs','status':'modified'}])
d='fx/cmp403'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,cmp_status=403)
d='fx/renamed'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=[{'filename':'ts/src/generated/New.d.ts','status':'renamed','previous_filename':'ts/src/generated/Old.d.ts'}])
d='fx/renamedfromscripts'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=[{'filename':'ts/src/generated/X.d.ts','status':'renamed','previous_filename':'scripts/check-generated-drift.sh'}])
# A head already on schemas main (coord fast-forward land, PR left open): not waiting.
d='fx/cmpbehind'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=[],cmp_state='behind')
# The status arm alone must skip it, even with files listed (lets a mutation flip it).
d='fx/cmpbehindfiles'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,cmp_state='behind')
d='fx/cmpidentical'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=[],cmp_state='identical')
# Landed by rebase: still compares diverged, and is absorbable (the re-check then adds nothing).
d='fx/cmpdiverged'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,cmp_state='diverged')
d='fx/cmpnostatus'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,cmp_state=None)
d='fx/nomergebase'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,mb=None)
d='fx/badmergebase'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,mb='not-a-sha')
# The feed must be newest-first; one that is not makes the 72 h bound a lie.
d='fx/notnewestfirst'; w(d,E+'1',[ev(285,L(2546),T),ev(285,L(2547),'2026-11-05T09:00:00Z')]); rpr(d,2546); rpr(d,2547); spr(d,285)
d='fx/pagesoutoforder'; w(d,E+'1',[ev(1,'x','2026-11-05T07:00:00Z',event='labeled')]*100); w(d,E+'2',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285)
# A partner that touches only the declared-wait toml changes no binding.
d='fx/tomlonly'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=[{'filename':'codegen/pending-registration.toml','status':'modified'}])
# A compare entry with no filename: the step's own UNKNOWN, not a bare set -e exit.
d='fx/nullfilename'; w(d,E+'1',[ev(285,L(2546),T)]); rpr(d,2546); spr(d,285,files=GEN+[{'status':'added'}])
