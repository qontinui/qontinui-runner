import{n as e}from"./chunk-BneVvdWh.js";import{t}from"./iframe-YYqiyj2T.js";import{n,t as r}from"./utils-CL72_TDw.js";import{a as i,l as a,r as o,s,t as c}from"./lucide-react-8q3HJ7j9.js";function l({icon:e,title:t,description:n,className:i}){return(0,u.jsxs)(`div`,{className:r(`flex flex-col items-center justify-center py-12 px-4`,i),children:[(0,u.jsx)(`div`,{className:`rounded-full bg-muted/50 p-3 mb-3`,children:(0,u.jsx)(e,{className:`h-6 w-6 text-muted-foreground`})}),(0,u.jsx)(`p`,{className:`text-sm font-medium text-foreground`,children:t}),n&&(0,u.jsx)(`p`,{className:`text-xs text-muted-foreground mt-1 text-center max-w-[200px]`,children:n})]})}var u,d=e((()=>{n(),u=t(),l.__docgenInfo={description:``,methods:[],displayName:`EmptyState`,props:{icon:{required:!0,tsType:{name:`LucideIcon`},description:``},title:{required:!0,tsType:{name:`string`},description:``},description:{required:!1,tsType:{name:`string`},description:``},className:{required:!1,tsType:{name:`string`},description:``}}}})),f,p,m,h,g,_,v,y,b,x,S,C,w,T,E;e((()=>{c(),d(),f=t(),x={title:`UI/Shared/EmptyState`,component:l,tags:[`autodocs`],parameters:{layout:`centered`,docs:{description:{component:`
Standardized empty state display for dashboard widgets. Provides a consistent
visual pattern when a widget has no data to show.

## Usage

\`\`\`tsx
import { EmptyState } from "./EmptyState";
import { Terminal } from "lucide-react";

<EmptyState
  icon={Terminal}
  title="No commands yet"
  description="Commands will appear here as they execute"
/>
\`\`\`
        `}}},argTypes:{icon:{control:!1,description:`Lucide icon component to display`},title:{control:`text`,description:`Title text displayed below the icon`},description:{control:`text`,description:`Optional description text below the title`},className:{control:`text`,description:`Additional CSS classes for custom sizing/spacing`}}},S={args:{icon:o,title:`No commands yet`,description:`Commands will appear here as they execute`}},C={args:{icon:s,title:`No active sessions`},parameters:{docs:{description:{story:`Empty state with just an icon and title, no description text`}}}},w={args:{icon:a,title:`No recent activity`,description:`Activity will be logged here`,className:`py-8 min-h-[200px]`},parameters:{docs:{description:{story:`Empty state with custom className for adjusted sizing and spacing`}}}},T={render:()=>(0,f.jsx)(`div`,{className:`grid grid-cols-2 gap-4`,children:[{icon:o,title:`No commands`,description:`Run a command to get started`},{icon:s,title:`No sessions`,description:`Start a session to begin`},{icon:a,title:`No history`,description:`History will appear here`},{icon:i,title:`No results`,description:`Try adjusting your search`}].map(({icon:e,title:t,description:n})=>(0,f.jsx)(`div`,{className:`border border-border rounded-lg`,children:(0,f.jsx)(l,{icon:e,title:t,description:n})},t))}),parameters:{docs:{description:{story:`Multiple empty states showing different icon and text combinations`}}}},S.parameters={...S.parameters,docs:{...(p=S.parameters)==null?void 0:p.docs,source:{originalSource:`{
  args: {
    icon: Terminal,
    title: "No commands yet",
    description: "Commands will appear here as they execute"
  }
}`,...(m=S.parameters)==null||(m=m.docs)==null?void 0:m.source}}},C.parameters={...C.parameters,docs:{...(h=C.parameters)==null?void 0:h.docs,source:{originalSource:`{
  args: {
    icon: Monitor,
    title: "No active sessions"
  },
  parameters: {
    docs: {
      description: {
        story: "Empty state with just an icon and title, no description text"
      }
    }
  }
}`,...(g=C.parameters)==null||(g=g.docs)==null?void 0:g.source}}},w.parameters={...w.parameters,docs:{...(_=w.parameters)==null?void 0:_.docs,source:{originalSource:`{
  args: {
    icon: Clock,
    title: "No recent activity",
    description: "Activity will be logged here",
    className: "py-8 min-h-[200px]"
  },
  parameters: {
    docs: {
      description: {
        story: "Empty state with custom className for adjusted sizing and spacing"
      }
    }
  }
}`,...(v=w.parameters)==null||(v=v.docs)==null?void 0:v.source}}},T.parameters={...T.parameters,docs:{...(y=T.parameters)==null?void 0:y.docs,source:{originalSource:`{
  render: () => {
    const examples = [{
      icon: Terminal,
      title: "No commands",
      description: "Run a command to get started"
    }, {
      icon: Monitor,
      title: "No sessions",
      description: "Start a session to begin"
    }, {
      icon: Clock,
      title: "No history",
      description: "History will appear here"
    }, {
      icon: Search,
      title: "No results",
      description: "Try adjusting your search"
    }] as const;
    return <div className="grid grid-cols-2 gap-4">\r
        {examples.map(({
        icon,
        title,
        description
      }) => <div key={title} className="border border-border rounded-lg">\r
            <EmptyState icon={icon} title={title} description={description} />\r
          </div>)}\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "Multiple empty states showing different icon and text combinations"
      }
    }
  }
}`,...(b=T.parameters)==null||(b=b.docs)==null?void 0:b.source}}},E=[`Default`,`WithoutDescription`,`CustomClassName`,`IconVariations`]}))();export{w as CustomClassName,S as Default,T as IconVariations,C as WithoutDescription,E as __namedExportsOrder,x as default};