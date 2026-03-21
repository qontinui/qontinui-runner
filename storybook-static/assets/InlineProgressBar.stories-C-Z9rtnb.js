import{n as e}from"./chunk-BneVvdWh.js";import{t}from"./iframe-YYqiyj2T.js";import{r as n,t as r}from"./ProgressBar-D3PQjj_0.js";var i,a,o,s,c,l,u,d,f,p,m,h,g,_,v,y,b,x,S,C,w,T,E,D,O,k,A,j,M,N,P,F,I,L,R,z,B,V,H,U,W,G;e((()=>{n(),i=t(),j={title:`UI/Progress/InlineProgressBar`,component:r,tags:[`autodocs`],parameters:{layout:`centered`,docs:{description:{component:`
A compact inline progress indicator designed for use in tables, lists, and other space-constrained contexts.

Features:
- **Fixed width** - 48px progress bar for consistent alignment
- **Inline layout** - Horizontal bar + count label
- **Same color system** - Uses the same semantic colors as ProgressBar
- **Indeterminate support** - Animated when total is null

## Usage

\`\`\`tsx
import { InlineProgressBar } from "@/components/ui/ProgressBar";

// In a table cell
<InlineProgressBar current={5} total={10} progressType="file_progress" />

// Unknown total (indeterminate)
<InlineProgressBar current={15} total={null} progressType="analysis_progress" />
\`\`\`
        `}}},argTypes:{current:{control:{type:`number`,min:0,max:100},description:`Current progress value`},total:{control:{type:`number`,min:0,max:100},description:`Total value (null for indeterminate)`},progressType:{control:`select`,options:[`default`,`file_progress`,`test_progress`,`analysis_progress`,`review_progress`,`iteration_progress`],description:`Progress type determines color scheme`},className:{control:`text`,description:`Additional CSS classes`}}},M={args:{current:5,total:10}},N={args:{current:75,total:100}},P={args:{current:10,total:10},parameters:{docs:{description:{story:`Shows success state (green) when current equals total`}}}},F={args:{current:0,total:10}},I={args:{current:15,total:null},parameters:{docs:{description:{story:`Animated indeterminate state when total is null. Only shows current value.`}}}},L={args:{current:12,total:50,progressType:`file_progress`},parameters:{docs:{description:{story:`Blue color for file operations`}}}},R={args:{current:8,total:20,progressType:`test_progress`},parameters:{docs:{description:{story:`Purple color for test execution`}}}},z={args:{current:3,total:10,progressType:`analysis_progress`},parameters:{docs:{description:{story:`Cyan color for analysis tasks`}}}},B={args:{current:5,total:15,progressType:`review_progress`},parameters:{docs:{description:{story:`Amber color for review items`}}}},V={args:{current:2,total:5,progressType:`iteration_progress`},parameters:{docs:{description:{story:`Emerald color for iteration cycles`}}}},H={render:()=>(0,i.jsx)(`div`,{className:`space-y-3`,children:[`default`,`file_progress`,`test_progress`,`analysis_progress`,`review_progress`,`iteration_progress`].map(e=>(0,i.jsxs)(`div`,{className:`flex items-center gap-4`,children:[(0,i.jsx)(`span`,{className:`text-xs text-muted-foreground w-32`,children:e}),(0,i.jsx)(r,{current:6,total:10,progressType:e})]},e))}),parameters:{docs:{description:{story:`All progress type colors shown side by side`}}}},U={render:()=>(0,i.jsxs)(`table`,{className:`w-full text-sm`,children:[(0,i.jsx)(`thead`,{children:(0,i.jsxs)(`tr`,{className:`border-b border-border/50`,children:[(0,i.jsx)(`th`,{className:`text-left py-2 px-3 text-muted-foreground font-medium`,children:`Task`}),(0,i.jsx)(`th`,{className:`text-left py-2 px-3 text-muted-foreground font-medium`,children:`Progress`})]})}),(0,i.jsx)(`tbody`,{children:[{name:`Process Files`,current:45,total:100,type:`file_progress`},{name:`Run Tests`,current:8,total:20,type:`test_progress`},{name:`Analyze Code`,current:3,total:null,type:`analysis_progress`},{name:`Review Changes`,current:15,total:15,type:`review_progress`}].map(e=>(0,i.jsxs)(`tr`,{className:`border-b border-border/30`,children:[(0,i.jsx)(`td`,{className:`py-2 px-3`,children:e.name}),(0,i.jsx)(`td`,{className:`py-2 px-3`,children:(0,i.jsx)(r,{current:e.current,total:e.total,progressType:e.type})})]},e.name))})]}),decorators:[e=>(0,i.jsx)(`div`,{style:{width:`400px`},className:`bg-card/50 rounded-lg`,children:(0,i.jsx)(e,{})})],parameters:{docs:{description:{story:`Example showing InlineProgressBar used within a table for multiple task progress tracking`}}}},W={render:()=>(0,i.jsx)(`div`,{className:`space-y-1`,children:[{id:1,name:`config.json`,current:1,total:1},{id:2,name:`index.ts`,current:50,total:100},{id:3,name:`utils.ts`,current:0,total:45},{id:4,name:`main.py`,current:12,total:null}].map(e=>(0,i.jsxs)(`div`,{className:`flex items-center justify-between py-1.5 px-2 hover:bg-muted/30 rounded`,children:[(0,i.jsx)(`span`,{className:`text-sm truncate`,children:e.name}),(0,i.jsx)(r,{current:e.current,total:e.total,progressType:`file_progress`})]},e.id))}),decorators:[e=>(0,i.jsx)(`div`,{style:{width:`280px`},children:(0,i.jsx)(e,{})})],parameters:{docs:{description:{story:`Example showing InlineProgressBar in a file list with various states`}}}},M.parameters={...M.parameters,docs:{...(a=M.parameters)==null?void 0:a.docs,source:{originalSource:`{
  args: {
    current: 5,
    total: 10
  }
}`,...(o=M.parameters)==null||(o=o.docs)==null?void 0:o.source}}},N.parameters={...N.parameters,docs:{...(s=N.parameters)==null?void 0:s.docs,source:{originalSource:`{
  args: {
    current: 75,
    total: 100
  }
}`,...(c=N.parameters)==null||(c=c.docs)==null?void 0:c.source}}},P.parameters={...P.parameters,docs:{...(l=P.parameters)==null?void 0:l.docs,source:{originalSource:`{
  args: {
    current: 10,
    total: 10
  },
  parameters: {
    docs: {
      description: {
        story: "Shows success state (green) when current equals total"
      }
    }
  }
}`,...(u=P.parameters)==null||(u=u.docs)==null?void 0:u.source}}},F.parameters={...F.parameters,docs:{...(d=F.parameters)==null?void 0:d.docs,source:{originalSource:`{
  args: {
    current: 0,
    total: 10
  }
}`,...(f=F.parameters)==null||(f=f.docs)==null?void 0:f.source}}},I.parameters={...I.parameters,docs:{...(p=I.parameters)==null?void 0:p.docs,source:{originalSource:`{
  args: {
    current: 15,
    total: null
  },
  parameters: {
    docs: {
      description: {
        story: "Animated indeterminate state when total is null. Only shows current value."
      }
    }
  }
}`,...(m=I.parameters)==null||(m=m.docs)==null?void 0:m.source}}},L.parameters={...L.parameters,docs:{...(h=L.parameters)==null?void 0:h.docs,source:{originalSource:`{
  args: {
    current: 12,
    total: 50,
    progressType: "file_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Blue color for file operations"
      }
    }
  }
}`,...(g=L.parameters)==null||(g=g.docs)==null?void 0:g.source}}},R.parameters={...R.parameters,docs:{...(_=R.parameters)==null?void 0:_.docs,source:{originalSource:`{
  args: {
    current: 8,
    total: 20,
    progressType: "test_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Purple color for test execution"
      }
    }
  }
}`,...(v=R.parameters)==null||(v=v.docs)==null?void 0:v.source}}},z.parameters={...z.parameters,docs:{...(y=z.parameters)==null?void 0:y.docs,source:{originalSource:`{
  args: {
    current: 3,
    total: 10,
    progressType: "analysis_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Cyan color for analysis tasks"
      }
    }
  }
}`,...(b=z.parameters)==null||(b=b.docs)==null?void 0:b.source}}},B.parameters={...B.parameters,docs:{...(x=B.parameters)==null?void 0:x.docs,source:{originalSource:`{
  args: {
    current: 5,
    total: 15,
    progressType: "review_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Amber color for review items"
      }
    }
  }
}`,...(S=B.parameters)==null||(S=S.docs)==null?void 0:S.source}}},V.parameters={...V.parameters,docs:{...(C=V.parameters)==null?void 0:C.docs,source:{originalSource:`{
  args: {
    current: 2,
    total: 5,
    progressType: "iteration_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Emerald color for iteration cycles"
      }
    }
  }
}`,...(w=V.parameters)==null||(w=w.docs)==null?void 0:w.source}}},H.parameters={...H.parameters,docs:{...(T=H.parameters)==null?void 0:T.docs,source:{originalSource:`{
  render: () => {
    const progressTypes: ProgressType[] = ["default", "file_progress", "test_progress", "analysis_progress", "review_progress", "iteration_progress"];
    return <div className="space-y-3">\r
        {progressTypes.map(type => <div key={type} className="flex items-center gap-4">\r
            <span className="text-xs text-muted-foreground w-32">{type}</span>\r
            <InlineProgressBar current={6} total={10} progressType={type} />\r
          </div>)}\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "All progress type colors shown side by side"
      }
    }
  }
}`,...(E=H.parameters)==null||(E=E.docs)==null?void 0:E.source}}},U.parameters={...U.parameters,docs:{...(D=U.parameters)==null?void 0:D.docs,source:{originalSource:`{
  render: () => {
    const rows = [{
      name: "Process Files",
      current: 45,
      total: 100,
      type: "file_progress" as const
    }, {
      name: "Run Tests",
      current: 8,
      total: 20,
      type: "test_progress" as const
    }, {
      name: "Analyze Code",
      current: 3,
      total: null,
      type: "analysis_progress" as const
    }, {
      name: "Review Changes",
      current: 15,
      total: 15,
      type: "review_progress" as const
    }];
    return <table className="w-full text-sm">\r
        <thead>\r
          <tr className="border-b border-border/50">\r
            <th className="text-left py-2 px-3 text-muted-foreground font-medium">Task</th>\r
            <th className="text-left py-2 px-3 text-muted-foreground font-medium">Progress</th>\r
          </tr>\r
        </thead>\r
        <tbody>\r
          {rows.map(row => <tr key={row.name} className="border-b border-border/30">\r
              <td className="py-2 px-3">{row.name}</td>\r
              <td className="py-2 px-3">\r
                <InlineProgressBar current={row.current} total={row.total} progressType={row.type} />\r
              </td>\r
            </tr>)}\r
        </tbody>\r
      </table>;
  },
  decorators: [Story => <div style={{
    width: "400px"
  }} className="bg-card/50 rounded-lg">\r
        <Story />\r
      </div>],
  parameters: {
    docs: {
      description: {
        story: "Example showing InlineProgressBar used within a table for multiple task progress tracking"
      }
    }
  }
}`,...(O=U.parameters)==null||(O=O.docs)==null?void 0:O.source}}},W.parameters={...W.parameters,docs:{...(k=W.parameters)==null?void 0:k.docs,source:{originalSource:`{
  render: () => {
    const items = [{
      id: 1,
      name: "config.json",
      current: 1,
      total: 1
    }, {
      id: 2,
      name: "index.ts",
      current: 50,
      total: 100
    }, {
      id: 3,
      name: "utils.ts",
      current: 0,
      total: 45
    }, {
      id: 4,
      name: "main.py",
      current: 12,
      total: null
    }];
    return <div className="space-y-1">\r
        {items.map(item => <div key={item.id} className="flex items-center justify-between py-1.5 px-2 hover:bg-muted/30 rounded">\r
            <span className="text-sm truncate">{item.name}</span>\r
            <InlineProgressBar current={item.current} total={item.total} progressType="file_progress" />\r
          </div>)}\r
      </div>;
  },
  decorators: [Story => <div style={{
    width: "280px"
  }}>\r
        <Story />\r
      </div>],
  parameters: {
    docs: {
      description: {
        story: "Example showing InlineProgressBar in a file list with various states"
      }
    }
  }
}`,...(A=W.parameters)==null||(A=A.docs)==null?void 0:A.source}}},G=[`Default`,`WithProgress`,`Complete`,`Empty`,`Indeterminate`,`FileProgress`,`TestProgress`,`AnalysisProgress`,`ReviewProgress`,`IterationProgress`,`AllProgressTypes`,`InTableContext`,`InListContext`]}))();export{H as AllProgressTypes,z as AnalysisProgress,P as Complete,M as Default,F as Empty,L as FileProgress,W as InListContext,U as InTableContext,I as Indeterminate,V as IterationProgress,B as ReviewProgress,R as TestProgress,N as WithProgress,G as __namedExportsOrder,j as default};