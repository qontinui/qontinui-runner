import{a as e,n as t}from"./chunk-BneVvdWh.js";import{n,t as r}from"./iframe-YYqiyj2T.js";import{n as i,r as a,t as o}from"./ProgressBar-D3PQjj_0.js";import{n as s,t as c}from"./StepProgressMarker-DyPRVHS-.js";function l(){let[e,t]=(0,u.useState)([{name:`document.pdf`,size:`2.4 MB`,progress:100,status:`complete`},{name:`image.png`,size:`1.2 MB`,progress:75,status:`uploading`},{name:`data.json`,size:`456 KB`,progress:30,status:`uploading`},{name:`report.xlsx`,size:`3.1 MB`,progress:0,status:`queued`}]);return(0,u.useEffect)(()=>{let e=setInterval(()=>{t(e=>e.map(t=>{if(t.status===`uploading`&&t.progress<100){let e=Math.min(100,t.progress+Math.random()*10);return{...t,progress:Math.round(e),status:e>=100?`complete`:`uploading`}}return t.status===`queued`&&e.every(e=>e.status!==`uploading`||e.progress===100)?{...t,status:`uploading`}:t}))},500);return()=>clearInterval(e)},[]),(0,d.jsxs)(`div`,{className:`bg-card/50 rounded-lg p-4 w-[400px]`,children:[(0,d.jsx)(`h3`,{className:`text-sm font-semibold mb-4`,children:`Uploading Files`}),(0,d.jsx)(`div`,{className:`space-y-3`,children:e.map(e=>(0,d.jsxs)(`div`,{className:`p-3 bg-muted/20 rounded-lg`,children:[(0,d.jsxs)(`div`,{className:`flex items-center justify-between mb-2`,children:[(0,d.jsxs)(`div`,{children:[(0,d.jsx)(`span`,{className:`text-sm font-medium block`,children:e.name}),(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground`,children:e.size})]}),(0,d.jsxs)(`span`,{className:`text-xs ${e.status===`complete`?`text-green-500`:e.status===`uploading`?`text-blue-400`:`text-muted-foreground`}`,children:[e.status===`complete`&&`Complete`,e.status===`uploading`&&`${e.progress}%`,e.status===`queued`&&`Queued`]})]}),(0,d.jsx)(i,{current:e.progress,total:100,size:`xs`,status:e.status===`complete`?`success`:e.status===`uploading`?`active`:`idle`,progressType:`file_progress`})]},e.name))})]})}var u,d,f,p,m,h,g,_,v,y,b,x,S,C,w,T,E,D,O,k,A,j,M;t((()=>{u=e(n(),1),a(),s(),d=r(),w=()=>(0,d.jsx)(`div`,{}),T={title:`UI/Progress/Showcase`,component:w,tags:[`autodocs`],parameters:{layout:`padded`,docs:{description:{component:`
# Progress Components Overview

This showcase presents all progress component variants together for visual comparison
and demonstrates real-world usage patterns.

## Components

| Component | Description | Use Case |
|-----------|-------------|----------|
| **ProgressBar** | Full-featured progress bar | Prominent progress displays |
| **InlineProgressBar** | Compact inline variant | Tables, lists, compact UI |
| **StepProgressIndicator** | Simplified step indicator | Step/workflow progress |

## Design System

All progress components share a consistent design language:

### Colors by Type
- **Default**: Blue
- **file_progress**: Blue
- **test_progress**: Purple
- **analysis_progress**: Cyan
- **review_progress**: Amber
- **iteration_progress**: Emerald

### Status Colors
- **Active**: Type color with pulse
- **Success**: Green
- **Error**: Red
- **Idle**: Muted
        `}}}},E={render:()=>(0,d.jsxs)(`div`,{className:`space-y-8`,children:[(0,d.jsxs)(`section`,{children:[(0,d.jsx)(`h2`,{className:`text-lg font-semibold mb-4`,children:`ProgressBar`}),(0,d.jsxs)(`div`,{className:`grid grid-cols-2 gap-6`,children:[(0,d.jsxs)(`div`,{className:`space-y-4`,children:[(0,d.jsx)(`h3`,{className:`text-sm font-medium text-muted-foreground`,children:`Sizes`}),[`xs`,`sm`,`md`,`lg`].map(e=>(0,d.jsxs)(`div`,{className:`flex items-center gap-3`,children:[(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground w-8`,children:e}),(0,d.jsx)(`div`,{className:`flex-1`,children:(0,d.jsx)(i,{current:60,total:100,size:e})})]},e))]}),(0,d.jsxs)(`div`,{className:`space-y-4`,children:[(0,d.jsx)(`h3`,{className:`text-sm font-medium text-muted-foreground`,children:`States`}),[`idle`,`active`,`success`,`error`].map(e=>(0,d.jsxs)(`div`,{className:`flex items-center gap-3`,children:[(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground w-12`,children:e}),(0,d.jsx)(`div`,{className:`flex-1`,children:(0,d.jsx)(i,{current:e===`idle`?0:e===`success`?100:60,total:100,status:e})})]},e))]})]})]}),(0,d.jsxs)(`section`,{children:[(0,d.jsx)(`h2`,{className:`text-lg font-semibold mb-4`,children:`Progress Types (Colors)`}),(0,d.jsx)(`div`,{className:`grid grid-cols-3 gap-4`,children:[`default`,`file_progress`,`test_progress`,`analysis_progress`,`review_progress`,`iteration_progress`].map(e=>(0,d.jsxs)(`div`,{className:`p-3 bg-muted/30 rounded-lg`,children:[(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground block mb-2`,children:e}),(0,d.jsx)(i,{current:65,total:100,progressType:e})]},e))})]}),(0,d.jsxs)(`section`,{children:[(0,d.jsx)(`h2`,{className:`text-lg font-semibold mb-4`,children:`InlineProgressBar`}),(0,d.jsx)(`div`,{className:`flex flex-wrap gap-6`,children:[`default`,`file_progress`,`test_progress`,`analysis_progress`].map(e=>(0,d.jsxs)(`div`,{className:`flex items-center gap-2`,children:[(0,d.jsx)(o,{current:7,total:12,progressType:e}),(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground`,children:e.replace(`_progress`,``)})]},e))})]}),(0,d.jsxs)(`section`,{children:[(0,d.jsx)(`h2`,{className:`text-lg font-semibold mb-4`,children:`StepProgressIndicator`}),(0,d.jsx)(`div`,{className:`flex flex-wrap gap-6`,children:[`file_progress`,`test_progress`,`analysis_progress`,`review_progress`,`iteration_progress`].map(e=>(0,d.jsx)(`div`,{className:`flex items-center gap-2`,children:(0,d.jsx)(c,{current:5,total:10,type:e})},e))})]})]}),parameters:{docs:{description:{story:`Complete overview of all progress component variants, sizes, and states`}}}},D={render:()=>{let e={current:7,total:12};return(0,d.jsxs)(`div`,{className:`space-y-6`,children:[(0,d.jsx)(`h2`,{className:`text-lg font-semibold`,children:`Same Data, Different Components`}),(0,d.jsxs)(`p`,{className:`text-sm text-muted-foreground`,children:[`All showing `,e.current,`/`,e.total,` progress`]}),(0,d.jsxs)(`div`,{className:`space-y-4`,children:[(0,d.jsxs)(`div`,{className:`flex items-center gap-4`,children:[(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground w-32`,children:`ProgressBar (md)`}),(0,d.jsx)(`div`,{className:`w-64`,children:(0,d.jsx)(i,{...e,showLabel:!0,labelFormat:`count`})})]}),(0,d.jsxs)(`div`,{className:`flex items-center gap-4`,children:[(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground w-32`,children:`ProgressBar (sm)`}),(0,d.jsx)(`div`,{className:`w-64`,children:(0,d.jsx)(i,{...e,size:`sm`,showLabel:!0,labelFormat:`count`})})]}),(0,d.jsxs)(`div`,{className:`flex items-center gap-4`,children:[(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground w-32`,children:`InlineProgressBar`}),(0,d.jsx)(o,{...e})]}),(0,d.jsxs)(`div`,{className:`flex items-center gap-4`,children:[(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground w-32`,children:`StepProgressIndicator`}),(0,d.jsx)(c,{...e,type:`default`})]})]})]})},parameters:{docs:{description:{story:`Direct comparison of different progress components with the same data`}}}},O={render:()=>{let e=[{id:1,name:`Initialize Environment`,status:`complete`,duration:`0.5s`},{id:2,name:`Load Configuration`,status:`complete`,duration:`0.2s`},{id:3,name:`Process Files`,status:`running`,progress:{current:45,total:100,type:`file_progress`}},{id:4,name:`Run Tests`,status:`pending`,progress:{current:0,total:20,type:`test_progress`}},{id:5,name:`Generate Report`,status:`pending`}],t={pending:`text-muted-foreground`,running:`text-blue-400`,complete:`text-green-500`,error:`text-red-500`};return(0,d.jsxs)(`div`,{className:`bg-card/50 rounded-lg p-4 w-[400px]`,children:[(0,d.jsx)(`h3`,{className:`text-sm font-semibold mb-4`,children:`Workflow Steps`}),(0,d.jsx)(`div`,{className:`space-y-2`,children:e.map(e=>(0,d.jsxs)(`div`,{className:`p-3 rounded-lg ${e.status===`running`?`bg-blue-500/10`:`bg-muted/20`}`,children:[(0,d.jsxs)(`div`,{className:`flex items-center justify-between mb-1`,children:[(0,d.jsx)(`span`,{className:`text-sm font-medium`,children:e.name}),(0,d.jsxs)(`span`,{className:`text-xs ${t[e.status]}`,children:[e.status===`complete`&&e.duration,e.status===`running`&&`In Progress`,e.status===`pending`&&`Pending`,e.status===`error`&&`Failed`]})]}),e.progress&&e.status!==`pending`&&(0,d.jsx)(i,{current:e.progress.current,total:e.progress.total,progressType:e.progress.type,size:`sm`,showLabel:!0,labelFormat:`both`})]},e.id))})]})},parameters:{docs:{description:{story:`Real-world example: Workflow step cards with integrated progress bars`}}}},k={render:()=>{let e=[{name:`File Processing`,current:156,total:200,type:`file_progress`},{name:`Test Execution`,current:45,total:120,type:`test_progress`},{name:`Code Analysis`,current:89,total:100,type:`analysis_progress`},{name:`Review Items`,current:12,total:30,type:`review_progress`}],t=e.reduce((e,t)=>e+t.current,0),n=e.reduce((e,t)=>e+t.total,0);return(0,d.jsxs)(`div`,{className:`bg-card rounded-lg p-4 w-[350px]`,children:[(0,d.jsxs)(`div`,{className:`flex items-center justify-between mb-4`,children:[(0,d.jsx)(`h3`,{className:`text-sm font-semibold`,children:`Overall Progress`}),(0,d.jsxs)(`span`,{className:`text-2xl font-bold text-primary`,children:[Math.round(t/n*100),`%`]})]}),(0,d.jsx)(i,{current:t,total:n,size:`lg`,className:`mb-6`}),(0,d.jsx)(`div`,{className:`space-y-3`,children:e.map(e=>(0,d.jsxs)(`div`,{className:`flex items-center justify-between`,children:[(0,d.jsx)(`span`,{className:`text-xs text-muted-foreground`,children:e.name}),(0,d.jsx)(o,{current:e.current,total:e.total,progressType:e.type})]},e.name))})]})},parameters:{docs:{description:{story:`Real-world example: Dashboard widget showing overall and task-level progress`}}}},A={render:()=>(0,d.jsx)(l,{}),parameters:{docs:{description:{story:`Real-world example: File upload progress with animated updates`}}}},j={render:()=>(0,d.jsxs)(`div`,{className:`space-y-6 w-[400px]`,children:[(0,d.jsx)(`h2`,{className:`text-lg font-semibold`,children:`Accessibility Features`}),(0,d.jsxs)(`div`,{className:`space-y-4`,children:[(0,d.jsxs)(`div`,{className:`p-4 bg-muted/20 rounded-lg`,children:[(0,d.jsx)(`h3`,{className:`text-sm font-medium mb-2`,children:`ARIA Attributes`}),(0,d.jsx)(`p`,{className:`text-xs text-muted-foreground mb-3`,children:`All progress bars include proper ARIA attributes:`}),(0,d.jsx)(`code`,{className:`text-xs block bg-muted/50 p-2 rounded`,children:`role="progressbar"
aria-valuenow={current}
aria-valuemin={0}
aria-valuemax={total}
aria-label="Progress: 50/100"
aria-busy={status === "active"}`})]}),(0,d.jsxs)(`div`,{className:`p-4 bg-muted/20 rounded-lg`,children:[(0,d.jsx)(`h3`,{className:`text-sm font-medium mb-2`,children:`Custom ARIA Label`}),(0,d.jsx)(i,{current:50,total:100,showLabel:!0,labelFormat:`count`,ariaLabel:`Uploading files: 50 of 100 complete`}),(0,d.jsxs)(`p`,{className:`text-xs text-muted-foreground mt-2`,children:[`Use `,(0,d.jsx)(`code`,{children:`ariaLabel`}),` prop for custom accessibility labels`]})]}),(0,d.jsxs)(`div`,{className:`p-4 bg-muted/20 rounded-lg`,children:[(0,d.jsx)(`h3`,{className:`text-sm font-medium mb-2`,children:`Indeterminate State`}),(0,d.jsx)(i,{current:42,indeterminate:!0,showLabel:!0,labelFormat:`count`}),(0,d.jsxs)(`p`,{className:`text-xs text-muted-foreground mt-2`,children:[`Indeterminate progress has `,(0,d.jsx)(`code`,{children:`aria-valuenow`}),` undefined`]})]})]})]}),parameters:{docs:{description:{story:`Demonstrates accessibility features including ARIA attributes`}}}},E.parameters={...E.parameters,docs:{...(f=E.parameters)==null?void 0:f.docs,source:{originalSource:`{
  render: () => {
    return <div className="space-y-8">\r
        {/* ProgressBar Section */}\r
        <section>\r
          <h2 className="text-lg font-semibold mb-4">ProgressBar</h2>\r
          <div className="grid grid-cols-2 gap-6">\r
            <div className="space-y-4">\r
              <h3 className="text-sm font-medium text-muted-foreground">Sizes</h3>\r
              {(["xs", "sm", "md", "lg"] as const).map(size => <div key={size} className="flex items-center gap-3">\r
                  <span className="text-xs text-muted-foreground w-8">{size}</span>\r
                  <div className="flex-1">\r
                    <ProgressBar current={60} total={100} size={size} />\r
                  </div>\r
                </div>)}\r
            </div>\r
            <div className="space-y-4">\r
              <h3 className="text-sm font-medium text-muted-foreground">States</h3>\r
              {(["idle", "active", "success", "error"] as const).map(status => <div key={status} className="flex items-center gap-3">\r
                  <span className="text-xs text-muted-foreground w-12">{status}</span>\r
                  <div className="flex-1">\r
                    <ProgressBar current={status === "idle" ? 0 : status === "success" ? 100 : 60} total={100} status={status} />\r
                  </div>\r
                </div>)}\r
            </div>\r
          </div>\r
        </section>\r
\r
        {/* Progress Types Section */}\r
        <section>\r
          <h2 className="text-lg font-semibold mb-4">Progress Types (Colors)</h2>\r
          <div className="grid grid-cols-3 gap-4">\r
            {(["default", "file_progress", "test_progress", "analysis_progress", "review_progress", "iteration_progress"] as const).map(type => <div key={type} className="p-3 bg-muted/30 rounded-lg">\r
                <span className="text-xs text-muted-foreground block mb-2">{type}</span>\r
                <ProgressBar current={65} total={100} progressType={type} />\r
              </div>)}\r
          </div>\r
        </section>\r
\r
        {/* InlineProgressBar Section */}\r
        <section>\r
          <h2 className="text-lg font-semibold mb-4">InlineProgressBar</h2>\r
          <div className="flex flex-wrap gap-6">\r
            {(["default", "file_progress", "test_progress", "analysis_progress"] as ProgressType[]).map(type => <div key={type} className="flex items-center gap-2">\r
                <InlineProgressBar current={7} total={12} progressType={type} />\r
                <span className="text-xs text-muted-foreground">\r
                  {type.replace("_progress", "")}\r
                </span>\r
              </div>)}\r
          </div>\r
        </section>\r
\r
        {/* StepProgressIndicator Section */}\r
        <section>\r
          <h2 className="text-lg font-semibold mb-4">StepProgressIndicator</h2>\r
          <div className="flex flex-wrap gap-6">\r
            {(["file_progress", "test_progress", "analysis_progress", "review_progress", "iteration_progress"] as const).map(type => <div key={type} className="flex items-center gap-2">\r
                <StepProgressIndicator current={5} total={10} type={type} />\r
              </div>)}\r
          </div>\r
        </section>\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "Complete overview of all progress component variants, sizes, and states"
      }
    }
  }
}`,...(p=E.parameters)==null||(p=p.docs)==null?void 0:p.source}}},D.parameters={...D.parameters,docs:{...(m=D.parameters)==null?void 0:m.docs,source:{originalSource:`{
  render: () => {
    const progress = {
      current: 7,
      total: 12
    };
    return <div className="space-y-6">\r
        <h2 className="text-lg font-semibold">Same Data, Different Components</h2>\r
        <p className="text-sm text-muted-foreground">\r
          All showing {progress.current}/{progress.total} progress\r
        </p>\r
\r
        <div className="space-y-4">\r
          <div className="flex items-center gap-4">\r
            <span className="text-xs text-muted-foreground w-32">ProgressBar (md)</span>\r
            <div className="w-64">\r
              <ProgressBar {...progress} showLabel labelFormat="count" />\r
            </div>\r
          </div>\r
\r
          <div className="flex items-center gap-4">\r
            <span className="text-xs text-muted-foreground w-32">ProgressBar (sm)</span>\r
            <div className="w-64">\r
              <ProgressBar {...progress} size="sm" showLabel labelFormat="count" />\r
            </div>\r
          </div>\r
\r
          <div className="flex items-center gap-4">\r
            <span className="text-xs text-muted-foreground w-32">InlineProgressBar</span>\r
            <InlineProgressBar {...progress} />\r
          </div>\r
\r
          <div className="flex items-center gap-4">\r
            <span className="text-xs text-muted-foreground w-32">StepProgressIndicator</span>\r
            <StepProgressIndicator {...progress} type="default" />\r
          </div>\r
        </div>\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "Direct comparison of different progress components with the same data"
      }
    }
  }
}`,...(h=D.parameters)==null||(h=h.docs)==null?void 0:h.source}}},O.parameters={...O.parameters,docs:{...(g=O.parameters)==null?void 0:g.docs,source:{originalSource:`{
  render: () => {
    interface Step {
      id: number;
      name: string;
      status: "pending" | "running" | "complete" | "error";
      progress?: {
        current: number;
        total: number | null;
        type: ProgressType;
      };
      duration?: string;
    }
    const steps: Step[] = [{
      id: 1,
      name: "Initialize Environment",
      status: "complete",
      duration: "0.5s"
    }, {
      id: 2,
      name: "Load Configuration",
      status: "complete",
      duration: "0.2s"
    }, {
      id: 3,
      name: "Process Files",
      status: "running",
      progress: {
        current: 45,
        total: 100,
        type: "file_progress"
      }
    }, {
      id: 4,
      name: "Run Tests",
      status: "pending",
      progress: {
        current: 0,
        total: 20,
        type: "test_progress"
      }
    }, {
      id: 5,
      name: "Generate Report",
      status: "pending"
    }];
    const statusColors: Record<string, string> = {
      pending: "text-muted-foreground",
      running: "text-blue-400",
      complete: "text-green-500",
      error: "text-red-500"
    };
    const _statusIcons: Record<string, string> = {
      pending: "circle",
      running: "loader",
      complete: "check-circle",
      error: "x-circle"
    };
    return <div className="bg-card/50 rounded-lg p-4 w-[400px]">\r
        <h3 className="text-sm font-semibold mb-4">Workflow Steps</h3>\r
        <div className="space-y-2">\r
          {steps.map(step => <div key={step.id} className={\`p-3 rounded-lg \${step.status === "running" ? "bg-blue-500/10" : "bg-muted/20"}\`}>\r
              <div className="flex items-center justify-between mb-1">\r
                <span className="text-sm font-medium">{step.name}</span>\r
                <span className={\`text-xs \${statusColors[step.status]}\`}>\r
                  {step.status === "complete" && step.duration}\r
                  {step.status === "running" && "In Progress"}\r
                  {step.status === "pending" && "Pending"}\r
                  {step.status === "error" && "Failed"}\r
                </span>\r
              </div>\r
              {step.progress && step.status !== "pending" && <ProgressBar current={step.progress.current} total={step.progress.total} progressType={step.progress.type} size="sm" showLabel labelFormat="both" />}\r
            </div>)}\r
        </div>\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "Real-world example: Workflow step cards with integrated progress bars"
      }
    }
  }
}`,...(_=O.parameters)==null||(_=_.docs)==null?void 0:_.source}}},k.parameters={...k.parameters,docs:{...(v=k.parameters)==null?void 0:v.docs,source:{originalSource:`{
  render: () => {
    interface Task {
      name: string;
      current: number;
      total: number;
      type: ProgressType;
    }
    const tasks: Task[] = [{
      name: "File Processing",
      current: 156,
      total: 200,
      type: "file_progress"
    }, {
      name: "Test Execution",
      current: 45,
      total: 120,
      type: "test_progress"
    }, {
      name: "Code Analysis",
      current: 89,
      total: 100,
      type: "analysis_progress"
    }, {
      name: "Review Items",
      current: 12,
      total: 30,
      type: "review_progress"
    }];
    const overallProgress = tasks.reduce((acc, t) => acc + t.current, 0);
    const overallTotal = tasks.reduce((acc, t) => acc + t.total, 0);
    return <div className="bg-card rounded-lg p-4 w-[350px]">\r
        <div className="flex items-center justify-between mb-4">\r
          <h3 className="text-sm font-semibold">Overall Progress</h3>\r
          <span className="text-2xl font-bold text-primary">\r
            {Math.round(overallProgress / overallTotal * 100)}%\r
          </span>\r
        </div>\r
\r
        <ProgressBar current={overallProgress} total={overallTotal} size="lg" className="mb-6" />\r
\r
        <div className="space-y-3">\r
          {tasks.map(task => <div key={task.name} className="flex items-center justify-between">\r
              <span className="text-xs text-muted-foreground">{task.name}</span>\r
              <InlineProgressBar current={task.current} total={task.total} progressType={task.type} />\r
            </div>)}\r
        </div>\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "Real-world example: Dashboard widget showing overall and task-level progress"
      }
    }
  }
}`,...(y=k.parameters)==null||(y=y.docs)==null?void 0:y.source}}},A.parameters={...A.parameters,docs:{...(b=A.parameters)==null?void 0:b.docs,source:{originalSource:`{
  render: () => <FileUploadExampleComponent />,
  parameters: {
    docs: {
      description: {
        story: "Real-world example: File upload progress with animated updates"
      }
    }
  }
}`,...(x=A.parameters)==null||(x=x.docs)==null?void 0:x.source}}},j.parameters={...j.parameters,docs:{...(S=j.parameters)==null?void 0:S.docs,source:{originalSource:`{
  render: () => {
    return <div className="space-y-6 w-[400px]">\r
        <h2 className="text-lg font-semibold">Accessibility Features</h2>\r
\r
        <div className="space-y-4">\r
          <div className="p-4 bg-muted/20 rounded-lg">\r
            <h3 className="text-sm font-medium mb-2">ARIA Attributes</h3>\r
            <p className="text-xs text-muted-foreground mb-3">\r
              All progress bars include proper ARIA attributes:\r
            </p>\r
            <code className="text-xs block bg-muted/50 p-2 rounded">\r
              {\`role="progressbar"
aria-valuenow={current}
aria-valuemin={0}
aria-valuemax={total}
aria-label="Progress: 50/100"
aria-busy={status === "active"}\`}\r
            </code>\r
          </div>\r
\r
          <div className="p-4 bg-muted/20 rounded-lg">\r
            <h3 className="text-sm font-medium mb-2">Custom ARIA Label</h3>\r
            <ProgressBar current={50} total={100} showLabel labelFormat="count" ariaLabel="Uploading files: 50 of 100 complete" />\r
            <p className="text-xs text-muted-foreground mt-2">\r
              Use <code>ariaLabel</code> prop for custom accessibility labels\r
            </p>\r
          </div>\r
\r
          <div className="p-4 bg-muted/20 rounded-lg">\r
            <h3 className="text-sm font-medium mb-2">Indeterminate State</h3>\r
            <ProgressBar current={42} indeterminate showLabel labelFormat="count" />\r
            <p className="text-xs text-muted-foreground mt-2">\r
              Indeterminate progress has <code>aria-valuenow</code> undefined\r
            </p>\r
          </div>\r
        </div>\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "Demonstrates accessibility features including ARIA attributes"
      }
    }
  }
}`,...(C=j.parameters)==null||(C=C.docs)==null?void 0:C.source}}},M=[`AllVariantsOverview`,`ComponentComparison`,`StepCardExample`,`DashboardWidgetExample`,`FileUploadExample`,`AccessibilityDemo`]}))();export{j as AccessibilityDemo,E as AllVariantsOverview,D as ComponentComparison,k as DashboardWidgetExample,A as FileUploadExample,O as StepCardExample,M as __namedExportsOrder,T as default};