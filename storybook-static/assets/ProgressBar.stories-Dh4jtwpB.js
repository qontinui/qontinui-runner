import{a as e,n as ee}from"./chunk-BneVvdWh.js";import{n as te,t as ne}from"./iframe-YYqiyj2T.js";import{n as t,r as re}from"./ProgressBar-D3PQjj_0.js";function ie(){let[e,ee]=(0,ae.useState)(0);return(0,ae.useEffect)(()=>{let e=setInterval(()=>{ee(e=>e>=100?0:e+5)},200);return()=>clearInterval(e)},[]),(0,n.jsx)(t,{current:e,total:100,showLabel:!0,labelFormat:`percentage`,progressType:`file_progress`,description:`Uploading files...`})}var ae,n,oe,r,se,i,ce,a,le,o,ue,s,de,c,fe,l,pe,u,me,d,he,f,ge,p,_e,m,ve,h,ye,g,be,_,xe,v,Se,y,Ce,b,we,x,Te,Ee,De,Oe,ke,Ae,je,Me,Ne,S,C,Pe,w,T,Fe,E,D,Ie,O,k,A,j,M,N,P,F,I,L,R,z,B,V,H,U,W,G,K,q,J,Y,X,Z,Q,$,Le;ee((()=>{ae=e(te(),1),re(),n=ne(),Ie={title:`UI/Progress/ProgressBar`,component:t,tags:[`autodocs`],parameters:{layout:`centered`,docs:{description:{component:`
A flexible progress bar component with support for:
- **Percentage or current/total display** - Show progress in different formats
- **Semantic colors** - Different colors based on progress type (files, tests, analysis, etc.)
- **Indeterminate mode** - Animated progress for unknown totals
- **Animated transitions** - Smooth width changes and pulsing effects
- **Success/error states** - Visual feedback for completed progress

## Usage

\`\`\`tsx
import { ProgressBar } from "@/components/ui/ProgressBar";

// Basic usage
<ProgressBar current={50} total={100} />

// With label
<ProgressBar current={5} total={10} showLabel labelFormat="count" />

// Indeterminate mode
<ProgressBar current={42} indeterminate />

// Different progress type
<ProgressBar current={3} total={10} progressType="test_progress" />
\`\`\`
        `}}},argTypes:{current:{control:{type:`number`,min:0,max:100},description:`Current progress value`},total:{control:{type:`number`,min:0,max:100},description:`Total value (if known)`},progressType:{control:`select`,options:[`default`,`file_progress`,`test_progress`,`analysis_progress`,`review_progress`,`iteration_progress`],description:`Progress type determines color scheme`},status:{control:`select`,options:[`idle`,`active`,`success`,`error`],description:`Override the auto-detected status`},indeterminate:{control:`boolean`,description:`Show indeterminate animation when total is unknown`},showLabel:{control:`boolean`,description:`Whether to show label`},labelFormat:{control:`select`,options:[`percentage`,`count`,`both`,`none`],description:`Label format`},label:{control:`text`,description:`Custom label text (overrides automatic label)`},description:{control:`text`,description:`Description text below progress bar`},size:{control:`select`,options:[`xs`,`sm`,`md`,`lg`],description:`Size variant`}},decorators:[e=>(0,n.jsx)(`div`,{style:{width:`300px`},children:(0,n.jsx)(e,{})})]},O={args:{current:50,total:100}},k={args:{current:50,total:100,showLabel:!0,labelFormat:`count`}},A={args:{current:75,total:100,showLabel:!0,labelFormat:`percentage`}},j={args:{current:50,total:100,showLabel:!0,labelFormat:`count`},parameters:{docs:{description:{story:`Shows progress as "current/total" format (e.g., "50/100")`}}}},M={args:{current:35,total:100,showLabel:!0,labelFormat:`both`},parameters:{docs:{description:{story:`Shows both count and percentage (e.g., "35/100 (35%)")`}}}},N={args:{current:25,total:100,showLabel:!0,labelFormat:`count`,description:`Processing files...`}},P={args:{current:42,total:100,label:`Almost halfway there!`}},F={args:{current:15,total:50,progressType:`file_progress`,showLabel:!0,labelFormat:`count`,description:`Processing files`},parameters:{docs:{description:{story:`Blue color scheme for file-related progress`}}}},I={args:{current:8,total:20,progressType:`test_progress`,showLabel:!0,labelFormat:`count`,description:`Running tests`},parameters:{docs:{description:{story:`Purple color scheme for test execution progress`}}}},L={args:{current:3,total:10,progressType:`analysis_progress`,showLabel:!0,labelFormat:`count`,description:`Analyzing code`},parameters:{docs:{description:{story:`Cyan color scheme for analysis tasks`}}}},R={args:{current:12,total:30,progressType:`review_progress`,showLabel:!0,labelFormat:`count`,description:`Review items`},parameters:{docs:{description:{story:`Amber color scheme for review progress`}}}},z={args:{current:2,total:5,progressType:`iteration_progress`,showLabel:!0,labelFormat:`both`,description:`Iteration cycle`},parameters:{docs:{description:{story:`Emerald color scheme for iteration/loop progress`}}}},B={args:{current:42,indeterminate:!0,showLabel:!0,labelFormat:`count`,description:`Loading...`},parameters:{docs:{description:{story:`Animated indeterminate state for when the total is unknown. The bar shows a sliding animation.`}}}},V={args:{current:15,total:null,showLabel:!0,labelFormat:`count`,description:`Total unknown`},parameters:{docs:{description:{story:`Automatically enters indeterminate mode when total is null`}}}},H={args:{current:60,total:100,status:`active`,showLabel:!0,labelFormat:`percentage`,description:`In progress`},parameters:{docs:{description:{story:`Active state shows a subtle pulse animation on the progress fill`}}}},U={args:{current:100,total:100,status:`success`,showLabel:!0,labelFormat:`count`,description:`Completed!`},parameters:{docs:{description:{story:`Success state with green color, auto-detected when current >= total`}}}},W={args:{current:45,total:100,status:`error`,showLabel:!0,labelFormat:`percentage`,description:`Failed at 45%`},parameters:{docs:{description:{story:`Error state with red color for failed operations`}}}},G={args:{current:0,total:100,status:`idle`,showLabel:!0,labelFormat:`percentage`,description:`Not started`},parameters:{docs:{description:{story:`Idle state before progress begins (current = 0)`}}}},K={args:{current:60,total:100,size:`xs`,showLabel:!0,labelFormat:`percentage`},parameters:{docs:{description:{story:`Extra small size (h-1) for compact UI elements`}}}},q={args:{current:60,total:100,size:`sm`,showLabel:!0,labelFormat:`percentage`},parameters:{docs:{description:{story:`Small size (h-1.5) for inline progress indicators`}}}},J={args:{current:60,total:100,size:`md`,showLabel:!0,labelFormat:`percentage`},parameters:{docs:{description:{story:`Medium size (h-2) - default size`}}}},Y={args:{current:60,total:100,size:`lg`,showLabel:!0,labelFormat:`percentage`},parameters:{docs:{description:{story:`Large size (h-3) for prominent progress displays`}}}},X={render:()=>(0,n.jsx)(ie,{}),parameters:{docs:{description:{story:`Demonstrates animated progress with smooth transitions as the value increments.`}}}},Z={render:()=>(0,n.jsx)(`div`,{className:`space-y-4 w-full`,children:[`default`,`file_progress`,`test_progress`,`analysis_progress`,`review_progress`,`iteration_progress`].map(e=>(0,n.jsxs)(`div`,{children:[(0,n.jsx)(`span`,{className:`text-xs text-muted-foreground mb-1 block`,children:e}),(0,n.jsx)(t,{current:65,total:100,progressType:e,showLabel:!0,labelFormat:`percentage`})]},e))}),decorators:[e=>(0,n.jsx)(`div`,{style:{width:`350px`},children:(0,n.jsx)(e,{})})],parameters:{docs:{description:{story:`Comparison of all available progress type color schemes`}}}},Q={render:()=>(0,n.jsx)(`div`,{className:`space-y-4 w-full`,children:[`xs`,`sm`,`md`,`lg`].map(e=>(0,n.jsxs)(`div`,{children:[(0,n.jsx)(`span`,{className:`text-xs text-muted-foreground mb-1 block`,children:e.toUpperCase()}),(0,n.jsx)(t,{current:60,total:100,size:e,showLabel:!0,labelFormat:`percentage`})]},e))}),decorators:[e=>(0,n.jsx)(`div`,{style:{width:`350px`},children:(0,n.jsx)(e,{})})],parameters:{docs:{description:{story:`Comparison of all available size variants`}}}},$={render:()=>(0,n.jsxs)(`div`,{className:`space-y-4 w-full`,children:[(0,n.jsxs)(`div`,{children:[(0,n.jsx)(`span`,{className:`text-xs text-muted-foreground mb-1 block`,children:`Idle (0%)`}),(0,n.jsx)(t,{current:0,total:100,showLabel:!0,labelFormat:`percentage`})]}),(0,n.jsxs)(`div`,{children:[(0,n.jsx)(`span`,{className:`text-xs text-muted-foreground mb-1 block`,children:`Active (pulse)`}),(0,n.jsx)(t,{current:45,total:100,status:`active`,showLabel:!0,labelFormat:`percentage`})]}),(0,n.jsxs)(`div`,{children:[(0,n.jsx)(`span`,{className:`text-xs text-muted-foreground mb-1 block`,children:`Indeterminate`}),(0,n.jsx)(t,{current:23,indeterminate:!0,showLabel:!0,labelFormat:`count`})]}),(0,n.jsxs)(`div`,{children:[(0,n.jsx)(`span`,{className:`text-xs text-muted-foreground mb-1 block`,children:`Success`}),(0,n.jsx)(t,{current:100,total:100,status:`success`,showLabel:!0,labelFormat:`percentage`})]}),(0,n.jsxs)(`div`,{children:[(0,n.jsx)(`span`,{className:`text-xs text-muted-foreground mb-1 block`,children:`Error`}),(0,n.jsx)(t,{current:67,total:100,status:`error`,showLabel:!0,labelFormat:`percentage`})]})]}),decorators:[e=>(0,n.jsx)(`div`,{style:{width:`350px`},children:(0,n.jsx)(e,{})})],parameters:{docs:{description:{story:`Comparison of all available states`}}}},O.parameters={...O.parameters,docs:{...(oe=O.parameters)==null?void 0:oe.docs,source:{originalSource:`{
  args: {
    current: 50,
    total: 100
  }
}`,...(r=O.parameters)==null||(r=r.docs)==null?void 0:r.source}}},k.parameters={...k.parameters,docs:{...(se=k.parameters)==null?void 0:se.docs,source:{originalSource:`{
  args: {
    current: 50,
    total: 100,
    showLabel: true,
    labelFormat: "count"
  }
}`,...(i=k.parameters)==null||(i=i.docs)==null?void 0:i.source}}},A.parameters={...A.parameters,docs:{...(ce=A.parameters)==null?void 0:ce.docs,source:{originalSource:`{
  args: {
    current: 75,
    total: 100,
    showLabel: true,
    labelFormat: "percentage"
  }
}`,...(a=A.parameters)==null||(a=a.docs)==null?void 0:a.source}}},j.parameters={...j.parameters,docs:{...(le=j.parameters)==null?void 0:le.docs,source:{originalSource:`{
  args: {
    current: 50,
    total: 100,
    showLabel: true,
    labelFormat: "count"
  },
  parameters: {
    docs: {
      description: {
        story: 'Shows progress as "current/total" format (e.g., "50/100")'
      }
    }
  }
}`,...(o=j.parameters)==null||(o=o.docs)==null?void 0:o.source}}},M.parameters={...M.parameters,docs:{...(ue=M.parameters)==null?void 0:ue.docs,source:{originalSource:`{
  args: {
    current: 35,
    total: 100,
    showLabel: true,
    labelFormat: "both"
  },
  parameters: {
    docs: {
      description: {
        story: 'Shows both count and percentage (e.g., "35/100 (35%)")'
      }
    }
  }
}`,...(s=M.parameters)==null||(s=s.docs)==null?void 0:s.source}}},N.parameters={...N.parameters,docs:{...(de=N.parameters)==null?void 0:de.docs,source:{originalSource:`{
  args: {
    current: 25,
    total: 100,
    showLabel: true,
    labelFormat: "count",
    description: "Processing files..."
  }
}`,...(c=N.parameters)==null||(c=c.docs)==null?void 0:c.source}}},P.parameters={...P.parameters,docs:{...(fe=P.parameters)==null?void 0:fe.docs,source:{originalSource:`{
  args: {
    current: 42,
    total: 100,
    label: "Almost halfway there!"
  }
}`,...(l=P.parameters)==null||(l=l.docs)==null?void 0:l.source}}},F.parameters={...F.parameters,docs:{...(pe=F.parameters)==null?void 0:pe.docs,source:{originalSource:`{
  args: {
    current: 15,
    total: 50,
    progressType: "file_progress",
    showLabel: true,
    labelFormat: "count",
    description: "Processing files"
  },
  parameters: {
    docs: {
      description: {
        story: "Blue color scheme for file-related progress"
      }
    }
  }
}`,...(u=F.parameters)==null||(u=u.docs)==null?void 0:u.source}}},I.parameters={...I.parameters,docs:{...(me=I.parameters)==null?void 0:me.docs,source:{originalSource:`{
  args: {
    current: 8,
    total: 20,
    progressType: "test_progress",
    showLabel: true,
    labelFormat: "count",
    description: "Running tests"
  },
  parameters: {
    docs: {
      description: {
        story: "Purple color scheme for test execution progress"
      }
    }
  }
}`,...(d=I.parameters)==null||(d=d.docs)==null?void 0:d.source}}},L.parameters={...L.parameters,docs:{...(he=L.parameters)==null?void 0:he.docs,source:{originalSource:`{
  args: {
    current: 3,
    total: 10,
    progressType: "analysis_progress",
    showLabel: true,
    labelFormat: "count",
    description: "Analyzing code"
  },
  parameters: {
    docs: {
      description: {
        story: "Cyan color scheme for analysis tasks"
      }
    }
  }
}`,...(f=L.parameters)==null||(f=f.docs)==null?void 0:f.source}}},R.parameters={...R.parameters,docs:{...(ge=R.parameters)==null?void 0:ge.docs,source:{originalSource:`{
  args: {
    current: 12,
    total: 30,
    progressType: "review_progress",
    showLabel: true,
    labelFormat: "count",
    description: "Review items"
  },
  parameters: {
    docs: {
      description: {
        story: "Amber color scheme for review progress"
      }
    }
  }
}`,...(p=R.parameters)==null||(p=p.docs)==null?void 0:p.source}}},z.parameters={...z.parameters,docs:{...(_e=z.parameters)==null?void 0:_e.docs,source:{originalSource:`{
  args: {
    current: 2,
    total: 5,
    progressType: "iteration_progress",
    showLabel: true,
    labelFormat: "both",
    description: "Iteration cycle"
  },
  parameters: {
    docs: {
      description: {
        story: "Emerald color scheme for iteration/loop progress"
      }
    }
  }
}`,...(m=z.parameters)==null||(m=m.docs)==null?void 0:m.source}}},B.parameters={...B.parameters,docs:{...(ve=B.parameters)==null?void 0:ve.docs,source:{originalSource:`{
  args: {
    current: 42,
    indeterminate: true,
    showLabel: true,
    labelFormat: "count",
    description: "Loading..."
  },
  parameters: {
    docs: {
      description: {
        story: "Animated indeterminate state for when the total is unknown. The bar shows a sliding animation."
      }
    }
  }
}`,...(h=B.parameters)==null||(h=h.docs)==null?void 0:h.source}}},V.parameters={...V.parameters,docs:{...(ye=V.parameters)==null?void 0:ye.docs,source:{originalSource:`{
  args: {
    current: 15,
    total: null,
    showLabel: true,
    labelFormat: "count",
    description: "Total unknown"
  },
  parameters: {
    docs: {
      description: {
        story: "Automatically enters indeterminate mode when total is null"
      }
    }
  }
}`,...(g=V.parameters)==null||(g=g.docs)==null?void 0:g.source}}},H.parameters={...H.parameters,docs:{...(be=H.parameters)==null?void 0:be.docs,source:{originalSource:`{
  args: {
    current: 60,
    total: 100,
    status: "active",
    showLabel: true,
    labelFormat: "percentage",
    description: "In progress"
  },
  parameters: {
    docs: {
      description: {
        story: "Active state shows a subtle pulse animation on the progress fill"
      }
    }
  }
}`,...(_=H.parameters)==null||(_=_.docs)==null?void 0:_.source}}},U.parameters={...U.parameters,docs:{...(xe=U.parameters)==null?void 0:xe.docs,source:{originalSource:`{
  args: {
    current: 100,
    total: 100,
    status: "success",
    showLabel: true,
    labelFormat: "count",
    description: "Completed!"
  },
  parameters: {
    docs: {
      description: {
        story: "Success state with green color, auto-detected when current >= total"
      }
    }
  }
}`,...(v=U.parameters)==null||(v=v.docs)==null?void 0:v.source}}},W.parameters={...W.parameters,docs:{...(Se=W.parameters)==null?void 0:Se.docs,source:{originalSource:`{
  args: {
    current: 45,
    total: 100,
    status: "error",
    showLabel: true,
    labelFormat: "percentage",
    description: "Failed at 45%"
  },
  parameters: {
    docs: {
      description: {
        story: "Error state with red color for failed operations"
      }
    }
  }
}`,...(y=W.parameters)==null||(y=y.docs)==null?void 0:y.source}}},G.parameters={...G.parameters,docs:{...(Ce=G.parameters)==null?void 0:Ce.docs,source:{originalSource:`{
  args: {
    current: 0,
    total: 100,
    status: "idle",
    showLabel: true,
    labelFormat: "percentage",
    description: "Not started"
  },
  parameters: {
    docs: {
      description: {
        story: "Idle state before progress begins (current = 0)"
      }
    }
  }
}`,...(b=G.parameters)==null||(b=b.docs)==null?void 0:b.source}}},K.parameters={...K.parameters,docs:{...(we=K.parameters)==null?void 0:we.docs,source:{originalSource:`{
  args: {
    current: 60,
    total: 100,
    size: "xs",
    showLabel: true,
    labelFormat: "percentage"
  },
  parameters: {
    docs: {
      description: {
        story: "Extra small size (h-1) for compact UI elements"
      }
    }
  }
}`,...(x=K.parameters)==null||(x=x.docs)==null?void 0:x.source}}},q.parameters={...q.parameters,docs:{...(Te=q.parameters)==null?void 0:Te.docs,source:{originalSource:`{
  args: {
    current: 60,
    total: 100,
    size: "sm",
    showLabel: true,
    labelFormat: "percentage"
  },
  parameters: {
    docs: {
      description: {
        story: "Small size (h-1.5) for inline progress indicators"
      }
    }
  }
}`,...(Ee=q.parameters)==null||(Ee=Ee.docs)==null?void 0:Ee.source}}},J.parameters={...J.parameters,docs:{...(De=J.parameters)==null?void 0:De.docs,source:{originalSource:`{
  args: {
    current: 60,
    total: 100,
    size: "md",
    showLabel: true,
    labelFormat: "percentage"
  },
  parameters: {
    docs: {
      description: {
        story: "Medium size (h-2) - default size"
      }
    }
  }
}`,...(Oe=J.parameters)==null||(Oe=Oe.docs)==null?void 0:Oe.source}}},Y.parameters={...Y.parameters,docs:{...(ke=Y.parameters)==null?void 0:ke.docs,source:{originalSource:`{
  args: {
    current: 60,
    total: 100,
    size: "lg",
    showLabel: true,
    labelFormat: "percentage"
  },
  parameters: {
    docs: {
      description: {
        story: "Large size (h-3) for prominent progress displays"
      }
    }
  }
}`,...(Ae=Y.parameters)==null||(Ae=Ae.docs)==null?void 0:Ae.source}}},X.parameters={...X.parameters,docs:{...(je=X.parameters)==null?void 0:je.docs,source:{originalSource:`{
  render: () => <AnimatedProgressComponent />,
  parameters: {
    docs: {
      description: {
        story: "Demonstrates animated progress with smooth transitions as the value increments."
      }
    }
  }
}`,...(Me=X.parameters)==null||(Me=Me.docs)==null?void 0:Me.source}}},Z.parameters={...Z.parameters,docs:{...(Ne=Z.parameters)==null?void 0:Ne.docs,source:{originalSource:`{
  render: () => {
    const progressTypes: ProgressType[] = ["default", "file_progress", "test_progress", "analysis_progress", "review_progress", "iteration_progress"];
    return <div className="space-y-4 w-full">\r
        {progressTypes.map(type => <div key={type}>\r
            <span className="text-xs text-muted-foreground mb-1 block">{type}</span>\r
            <ProgressBar current={65} total={100} progressType={type} showLabel labelFormat="percentage" />\r
          </div>)}\r
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
        story: "Comparison of all available progress type color schemes"
      }
    }
  }
}`,...(S=Z.parameters)==null||(S=S.docs)==null?void 0:S.source},description:{story:`Story showing all progress types side by side.`,...(C=Z.parameters)==null||(C=C.docs)==null?void 0:C.description}}},Q.parameters={...Q.parameters,docs:{...(Pe=Q.parameters)==null?void 0:Pe.docs,source:{originalSource:`{
  render: () => {
    const sizes = ["xs", "sm", "md", "lg"] as const;
    return <div className="space-y-4 w-full">\r
        {sizes.map(size => <div key={size}>\r
            <span className="text-xs text-muted-foreground mb-1 block">{size.toUpperCase()}</span>\r
            <ProgressBar current={60} total={100} size={size} showLabel labelFormat="percentage" />\r
          </div>)}\r
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
        story: "Comparison of all available size variants"
      }
    }
  }
}`,...(w=Q.parameters)==null||(w=w.docs)==null?void 0:w.source},description:{story:`Story showing all sizes side by side.`,...(T=Q.parameters)==null||(T=T.docs)==null?void 0:T.description}}},$.parameters={...$.parameters,docs:{...(Fe=$.parameters)==null?void 0:Fe.docs,source:{originalSource:`{
  render: () => {
    return <div className="space-y-4 w-full">\r
        <div>\r
          <span className="text-xs text-muted-foreground mb-1 block">Idle (0%)</span>\r
          <ProgressBar current={0} total={100} showLabel labelFormat="percentage" />\r
        </div>\r
        <div>\r
          <span className="text-xs text-muted-foreground mb-1 block">Active (pulse)</span>\r
          <ProgressBar current={45} total={100} status="active" showLabel labelFormat="percentage" />\r
        </div>\r
        <div>\r
          <span className="text-xs text-muted-foreground mb-1 block">Indeterminate</span>\r
          <ProgressBar current={23} indeterminate showLabel labelFormat="count" />\r
        </div>\r
        <div>\r
          <span className="text-xs text-muted-foreground mb-1 block">Success</span>\r
          <ProgressBar current={100} total={100} status="success" showLabel labelFormat="percentage" />\r
        </div>\r
        <div>\r
          <span className="text-xs text-muted-foreground mb-1 block">Error</span>\r
          <ProgressBar current={67} total={100} status="error" showLabel labelFormat="percentage" />\r
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
        story: "Comparison of all available states"
      }
    }
  }
}`,...(E=$.parameters)==null||(E=E.docs)==null?void 0:E.source},description:{story:`Story showing all states side by side.`,...(D=$.parameters)==null||(D=D.docs)==null?void 0:D.description}}},Le=`Default.WithLabel.Percentage.CountLabel.BothFormats.WithDescription.CustomLabel.FileProgress.TestProgress.AnalysisProgress.ReviewProgress.IterationProgress.Indeterminate.IndeterminateWithNullTotal.Active.Success.Error.Idle.ExtraSmall.Small.Medium.Large.Animated.AllProgressTypes.AllSizes.AllStates`.split(`.`)}))();export{H as Active,Z as AllProgressTypes,Q as AllSizes,$ as AllStates,L as AnalysisProgress,X as Animated,M as BothFormats,j as CountLabel,P as CustomLabel,O as Default,W as Error,K as ExtraSmall,F as FileProgress,G as Idle,B as Indeterminate,V as IndeterminateWithNullTotal,z as IterationProgress,Y as Large,J as Medium,A as Percentage,R as ReviewProgress,q as Small,U as Success,I as TestProgress,N as WithDescription,k as WithLabel,Le as __namedExportsOrder,Ie as default};