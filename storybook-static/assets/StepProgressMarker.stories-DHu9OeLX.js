import{n as e}from"./chunk-BneVvdWh.js";import{t}from"./iframe-YYqiyj2T.js";import{n,t as r}from"./StepProgressMarker-DyPRVHS-.js";var i,a,o,s,c,l,u,d,f,p,m,h,g,_,v,y,b,x,S,C,w,T,E,D,O,k,A,j,M,N,P,F,I,L,R,z,B,V,H,U;e((()=>{n(),i=t(),A={title:`UI/Progress/StepProgressIndicator`,component:r,tags:[`autodocs`],parameters:{layout:`centered`,docs:{description:{component:`
A simplified inline progress indicator for step rows. This is a wrapper around InlineProgressBar
with semantic mapping from marker types to progress colors.

**StepProgressMarker** - Full component that:
- Fetches progress data from the API
- Auto-refreshes for running steps
- Shows loading states
- Displays both compact and full modes

**StepProgressIndicator** - Simplified component that:
- Takes progress values directly
- Maps marker types to colors
- Purely presentational

## Marker Types and Colors

| Marker Type | Color | Use Case |
|-------------|-------|----------|
| file_progress | Blue | File operations |
| test_progress | Purple | Test execution |
| analysis_progress | Cyan | Code analysis |
| review_progress | Amber | Review items |
| iteration_progress | Emerald | Iteration cycles |

## Usage

\`\`\`tsx
import { StepProgressIndicator, StepProgressMarker } from "./StepProgressMarker";

// Simple indicator (presentational)
<StepProgressIndicator current={5} total={10} type="file_progress" />

// Full marker with API fetching (for running steps)
<StepProgressMarker
  taskRunId="run-123"
  checkpointId="step-1"
  autoRefresh
  compact
/>
\`\`\`
        `}}},argTypes:{current:{control:{type:`number`,min:0,max:100},description:`Current progress value`},total:{control:{type:`number`,min:0,max:100},description:`Total value (null for indeterminate)`},type:{control:`select`,options:[`file_progress`,`test_progress`,`analysis_progress`,`review_progress`,`iteration_progress`],description:`Marker type determines color`},className:{control:`text`,description:`Additional CSS classes`}}},j={args:{current:5,total:10}},M={args:{current:25,total:100,type:`file_progress`},parameters:{docs:{description:{story:`Blue progress bar for file operations`}}}},N={args:{current:8,total:20,type:`test_progress`},parameters:{docs:{description:{story:`Purple progress bar for test execution`}}}},P={args:{current:3,total:null,type:`analysis_progress`},parameters:{docs:{description:{story:`Cyan indeterminate progress for analysis (unknown total)`}}}},F={args:{current:12,total:15,type:`review_progress`},parameters:{docs:{description:{story:`Amber progress bar for review items`}}}},I={args:{current:2,total:5,type:`iteration_progress`},parameters:{docs:{description:{story:`Emerald progress bar for iteration cycles`}}}},L={args:{current:10,total:10,type:`test_progress`},parameters:{docs:{description:{story:`Completed progress (success state - green)`}}}},R={args:{current:42,total:null,type:`file_progress`},parameters:{docs:{description:{story:`Indeterminate progress when total is unknown`}}}},z={render:()=>(0,i.jsx)(`div`,{className:`space-y-3`,children:[{type:`file_progress`,label:`Files`},{type:`test_progress`,label:`Tests`},{type:`analysis_progress`,label:`Analysis`},{type:`review_progress`,label:`Review`},{type:`iteration_progress`,label:`Iteration`}].map(({type:e,label:t})=>(0,i.jsxs)(`div`,{className:`flex items-center gap-4`,children:[(0,i.jsx)(`span`,{className:`text-xs text-muted-foreground w-24`,children:t}),(0,i.jsx)(r,{current:6,total:10,type:e})]},e))}),parameters:{docs:{description:{story:`All marker types with their semantic colors`}}}},B={render:()=>{let e=[{id:1,name:`Setup Environment`,status:`complete`,current:5,total:5,type:`file_progress`},{id:2,name:`Run Tests`,status:`running`,current:8,total:20,type:`test_progress`},{id:3,name:`Analyze Results`,status:`pending`,current:0,total:10,type:`analysis_progress`},{id:4,name:`Generate Report`,status:`pending`,current:0,total:null,type:`review_progress`}],t=e=>{let t={complete:`bg-green-500/10 text-green-500`,running:`bg-blue-500/10 text-blue-400`,pending:`bg-muted/50 text-muted-foreground`};return t[e]||t.pending};return(0,i.jsx)(`div`,{className:`space-y-1`,children:e.map(e=>(0,i.jsxs)(`div`,{className:`flex items-center gap-3 py-2 px-3 rounded hover:bg-muted/30`,children:[(0,i.jsx)(`span`,{className:`text-sm font-medium flex-1`,children:e.name}),(0,i.jsx)(`span`,{className:`text-xs px-2 py-0.5 rounded ${t(e.status)}`,children:e.status}),(e.status===`running`||e.status===`complete`)&&(0,i.jsx)(r,{current:e.current,total:e.total,type:e.type})]},e.id))})},decorators:[e=>(0,i.jsx)(`div`,{style:{width:`450px`},className:`bg-card/50 rounded-lg p-2`,children:(0,i.jsx)(e,{})})],parameters:{docs:{description:{story:`Example showing StepProgressIndicator integrated into a step list UI`}}}},V={render:()=>{let e=[{type:`file_progress`,current:45,total:100,description:`Processing source files`},{type:`test_progress`,current:8,total:20,description:`Running unit tests`},{type:`analysis_progress`,current:12,total:null,description:`Analyzing dependencies`}],t=e=>({file_progress:`Files`,test_progress:`Tests`,analysis_progress:`Analysis`})[e]||e;return(0,i.jsx)(`div`,{className:`space-y-4`,children:e.map(e=>(0,i.jsxs)(`div`,{className:`space-y-1`,children:[(0,i.jsx)(`span`,{className:`text-xs font-medium`,children:t(e.type)}),(0,i.jsxs)(`div`,{className:`flex items-center gap-2`,children:[(0,i.jsx)(`div`,{className:`flex-1`,children:(0,i.jsx)(r,{current:e.current,total:e.total,type:e.type})}),e.description&&(0,i.jsx)(`span`,{className:`text-xs text-muted-foreground`,children:e.description})]})]},e.type))})},decorators:[e=>(0,i.jsx)(`div`,{style:{width:`400px`},children:(0,i.jsx)(e,{})})],parameters:{docs:{description:{story:`
Simulates the full StepProgressMarker display with:
- Type labels
- Progress bars with counts
- Optional descriptions

Note: The actual StepProgressMarker component fetches data from the API endpoint
\`/task-runs/{taskRunId}/steps/{checkpointId}/progress\`
        `}}}},H={render:()=>{let e={current:25,total:100,type:`test_progress`};return(0,i.jsxs)(`div`,{className:`space-y-6`,children:[(0,i.jsxs)(`div`,{children:[(0,i.jsx)(`h4`,{className:`text-xs font-medium text-muted-foreground mb-2`,children:`Compact Mode`}),(0,i.jsxs)(`div`,{className:`flex items-center gap-2 bg-muted/30 p-2 rounded`,children:[(0,i.jsx)(r,{...e}),(0,i.jsx)(`span`,{className:`text-xs text-muted-foreground`,children:`Tests`})]})]}),(0,i.jsxs)(`div`,{children:[(0,i.jsx)(`h4`,{className:`text-xs font-medium text-muted-foreground mb-2`,children:`Full Mode`}),(0,i.jsxs)(`div`,{className:`bg-muted/30 p-3 rounded space-y-1`,children:[(0,i.jsx)(`span`,{className:`text-xs font-medium`,children:`Tests`}),(0,i.jsxs)(`div`,{className:`flex items-center gap-2`,children:[(0,i.jsx)(`div`,{className:`flex-1`,children:(0,i.jsx)(r,{...e})}),(0,i.jsx)(`span`,{className:`text-xs text-muted-foreground`,children:`Running test suite`})]})]})]})]})},decorators:[e=>(0,i.jsx)(`div`,{style:{width:`350px`},children:(0,i.jsx)(e,{})})],parameters:{docs:{description:{story:`
Comparison of compact and full display modes:
- **Compact**: Single line with inline progress and type label
- **Full**: Header label, progress bar, and description
        `}}}},j.parameters={...j.parameters,docs:{...(a=j.parameters)==null?void 0:a.docs,source:{originalSource:`{
  args: {
    current: 5,
    total: 10
  }
}`,...(o=j.parameters)==null||(o=o.docs)==null?void 0:o.source}}},M.parameters={...M.parameters,docs:{...(s=M.parameters)==null?void 0:s.docs,source:{originalSource:`{
  args: {
    current: 25,
    total: 100,
    type: "file_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Blue progress bar for file operations"
      }
    }
  }
}`,...(c=M.parameters)==null||(c=c.docs)==null?void 0:c.source}}},N.parameters={...N.parameters,docs:{...(l=N.parameters)==null?void 0:l.docs,source:{originalSource:`{
  args: {
    current: 8,
    total: 20,
    type: "test_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Purple progress bar for test execution"
      }
    }
  }
}`,...(u=N.parameters)==null||(u=u.docs)==null?void 0:u.source}}},P.parameters={...P.parameters,docs:{...(d=P.parameters)==null?void 0:d.docs,source:{originalSource:`{
  args: {
    current: 3,
    total: null,
    type: "analysis_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Cyan indeterminate progress for analysis (unknown total)"
      }
    }
  }
}`,...(f=P.parameters)==null||(f=f.docs)==null?void 0:f.source}}},F.parameters={...F.parameters,docs:{...(p=F.parameters)==null?void 0:p.docs,source:{originalSource:`{
  args: {
    current: 12,
    total: 15,
    type: "review_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Amber progress bar for review items"
      }
    }
  }
}`,...(m=F.parameters)==null||(m=m.docs)==null?void 0:m.source}}},I.parameters={...I.parameters,docs:{...(h=I.parameters)==null?void 0:h.docs,source:{originalSource:`{
  args: {
    current: 2,
    total: 5,
    type: "iteration_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Emerald progress bar for iteration cycles"
      }
    }
  }
}`,...(g=I.parameters)==null||(g=g.docs)==null?void 0:g.source}}},L.parameters={...L.parameters,docs:{...(_=L.parameters)==null?void 0:_.docs,source:{originalSource:`{
  args: {
    current: 10,
    total: 10,
    type: "test_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Completed progress (success state - green)"
      }
    }
  }
}`,...(v=L.parameters)==null||(v=v.docs)==null?void 0:v.source}}},R.parameters={...R.parameters,docs:{...(y=R.parameters)==null?void 0:y.docs,source:{originalSource:`{
  args: {
    current: 42,
    total: null,
    type: "file_progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Indeterminate progress when total is unknown"
      }
    }
  }
}`,...(b=R.parameters)==null||(b=b.docs)==null?void 0:b.source}}},z.parameters={...z.parameters,docs:{...(x=z.parameters)==null?void 0:x.docs,source:{originalSource:`{
  render: () => {
    const types = [{
      type: "file_progress",
      label: "Files"
    }, {
      type: "test_progress",
      label: "Tests"
    }, {
      type: "analysis_progress",
      label: "Analysis"
    }, {
      type: "review_progress",
      label: "Review"
    }, {
      type: "iteration_progress",
      label: "Iteration"
    }] as const;
    return <div className="space-y-3">\r
        {types.map(({
        type,
        label
      }) => <div key={type} className="flex items-center gap-4">\r
            <span className="text-xs text-muted-foreground w-24">{label}</span>\r
            <StepProgressIndicator current={6} total={10} type={type} />\r
          </div>)}\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "All marker types with their semantic colors"
      }
    }
  }
}`,...(S=z.parameters)==null||(S=S.docs)==null?void 0:S.source}}},B.parameters={...B.parameters,docs:{...(C=B.parameters)==null?void 0:C.docs,source:{originalSource:`{
  render: () => {
    const steps = [{
      id: 1,
      name: "Setup Environment",
      status: "complete",
      current: 5,
      total: 5,
      type: "file_progress"
    }, {
      id: 2,
      name: "Run Tests",
      status: "running",
      current: 8,
      total: 20,
      type: "test_progress"
    }, {
      id: 3,
      name: "Analyze Results",
      status: "pending",
      current: 0,
      total: 10,
      type: "analysis_progress"
    }, {
      id: 4,
      name: "Generate Report",
      status: "pending",
      current: 0,
      total: null,
      type: "review_progress"
    }] as const;
    const getStatusBadge = (status: string) => {
      const styles: Record<string, string> = {
        complete: "bg-green-500/10 text-green-500",
        running: "bg-blue-500/10 text-blue-400",
        pending: "bg-muted/50 text-muted-foreground"
      };
      return styles[status] || styles.pending;
    };
    return <div className="space-y-1">\r
        {steps.map(step => <div key={step.id} className="flex items-center gap-3 py-2 px-3 rounded hover:bg-muted/30">\r
            <span className="text-sm font-medium flex-1">{step.name}</span>\r
            <span className={\`text-xs px-2 py-0.5 rounded \${getStatusBadge(step.status)}\`}>\r
              {step.status}\r
            </span>\r
            {(step.status === "running" || step.status === "complete") && <StepProgressIndicator current={step.current} total={step.total} type={step.type} />}\r
          </div>)}\r
      </div>;
  },
  decorators: [Story => <div style={{
    width: "450px"
  }} className="bg-card/50 rounded-lg p-2">\r
        <Story />\r
      </div>],
  parameters: {
    docs: {
      description: {
        story: "Example showing StepProgressIndicator integrated into a step list UI"
      }
    }
  }
}`,...(w=B.parameters)==null||(w=w.docs)==null?void 0:w.source}}},V.parameters={...V.parameters,docs:{...(T=V.parameters)==null?void 0:T.docs,source:{originalSource:`{
  render: () => {
    // Simulated progress marker data
    const mockMarkers: Array<{
      type: string;
      current: number;
      total: number | null;
      description: string | null;
    }> = [{
      type: "file_progress",
      current: 45,
      total: 100,
      description: "Processing source files"
    }, {
      type: "test_progress",
      current: 8,
      total: 20,
      description: "Running unit tests"
    }, {
      type: "analysis_progress",
      current: 12,
      total: null,
      description: "Analyzing dependencies"
    }];
    const getLabel = (type: string) => {
      const labels: Record<string, string> = {
        file_progress: "Files",
        test_progress: "Tests",
        analysis_progress: "Analysis"
      };
      return labels[type] || type;
    };
    return <div className="space-y-4">\r
        {mockMarkers.map(marker => <div key={marker.type} className="space-y-1">\r
            <span className="text-xs font-medium">{getLabel(marker.type)}</span>\r
            <div className="flex items-center gap-2">\r
              <div className="flex-1">\r
                <StepProgressIndicator current={marker.current} total={marker.total} type={marker.type} />\r
              </div>\r
              {marker.description && <span className="text-xs text-muted-foreground">{marker.description}</span>}\r
            </div>\r
          </div>)}\r
      </div>;
  },
  decorators: [Story => <div style={{
    width: "400px"
  }}>\r
        <Story />\r
      </div>],
  parameters: {
    docs: {
      description: {
        story: \`
Simulates the full StepProgressMarker display with:
- Type labels
- Progress bars with counts
- Optional descriptions

Note: The actual StepProgressMarker component fetches data from the API endpoint
\\\`/task-runs/{taskRunId}/steps/{checkpointId}/progress\\\`
        \`
      }
    }
  }
}`,...(E=V.parameters)==null||(E=E.docs)==null?void 0:E.source},description:{story:`This simulates what StepProgressMarker would display with real data.\r
The actual component fetches from API, but this shows the visual output.`,...(D=V.parameters)==null||(D=D.docs)==null?void 0:D.description}}},H.parameters={...H.parameters,docs:{...(O=H.parameters)==null?void 0:O.docs,source:{originalSource:`{
  render: () => {
    const data = {
      current: 25,
      total: 100,
      type: "test_progress" as const
    };
    return <div className="space-y-6">\r
        <div>\r
          <h4 className="text-xs font-medium text-muted-foreground mb-2">Compact Mode</h4>\r
          <div className="flex items-center gap-2 bg-muted/30 p-2 rounded">\r
            <StepProgressIndicator {...data} />\r
            <span className="text-xs text-muted-foreground">Tests</span>\r
          </div>\r
        </div>\r
\r
        <div>\r
          <h4 className="text-xs font-medium text-muted-foreground mb-2">Full Mode</h4>\r
          <div className="bg-muted/30 p-3 rounded space-y-1">\r
            <span className="text-xs font-medium">Tests</span>\r
            <div className="flex items-center gap-2">\r
              <div className="flex-1">\r
                <StepProgressIndicator {...data} />\r
              </div>\r
              <span className="text-xs text-muted-foreground">Running test suite</span>\r
            </div>\r
          </div>\r
        </div>\r
      </div>;
  },
  decorators: [Story => <div style={{
    width: "350px"
  }}>\r
        <Story />\r
      </div>],
  parameters: {
    docs: {
      description: {
        story: \`
Comparison of compact and full display modes:
- **Compact**: Single line with inline progress and type label
- **Full**: Header label, progress bar, and description
        \`
      }
    }
  }
}`,...(k=H.parameters)==null||(k=k.docs)==null?void 0:k.source}}},U=[`Default`,`FileProgress`,`TestProgress`,`AnalysisProgress`,`ReviewProgress`,`IterationProgress`,`Complete`,`Indeterminate`,`AllMarkerTypes`,`InStepRowContext`,`MockFullDisplay`,`CompactVsFull`]}))();export{z as AllMarkerTypes,P as AnalysisProgress,H as CompactVsFull,L as Complete,j as Default,M as FileProgress,B as InStepRowContext,R as Indeterminate,I as IterationProgress,V as MockFullDisplay,F as ReviewProgress,N as TestProgress,U as __namedExportsOrder,A as default};