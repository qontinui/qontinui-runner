import{n as e}from"./chunk-BneVvdWh.js";import{t}from"./iframe-YYqiyj2T.js";import{n,t as r}from"./utils-CL72_TDw.js";import{n as i,t as a}from"./colors-CaMkseCC.js";import{i as o,n as s,o as c,t as l,u}from"./lucide-react-8q3HJ7j9.js";function d({label:e,color:t,icon:n,className:a}){let o=i(t);return(0,f.jsxs)(`span`,{className:r(`inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-xs font-medium font-mono`,`bg-transparent`,o.text,o.border,a),children:[n,e]})}var f,p=e((()=>{n(),a(),f=t(),d.__docgenInfo={description:``,methods:[],displayName:`TypeBadge`,props:{label:{required:!0,tsType:{name:`string`},description:``},color:{required:!0,tsType:{name:`AccentColor`},description:``},icon:{required:!1,tsType:{name:`ReactReactNode`,raw:`React.ReactNode`},description:``},className:{required:!1,tsType:{name:`string`},description:``}}}})),m,h,g,_,v,y,b,x,S,C,w,T;e((()=>{l(),p(),m=t(),x={title:`UI/Shared/TypeBadge`,component:d,tags:[`autodocs`],parameters:{layout:`centered`,docs:{description:{component:`
Standardized outline badge for displaying step/action types. Uses the design
system's accent colors with a border-only (outline) style, distinct from
ModeBadge which uses a filled background.

## Usage

\`\`\`tsx
import { TypeBadge } from "./TypeBadge";
import { CheckCircle } from "lucide-react";

<TypeBadge label="command" color="blue" />
<TypeBadge label="verification" color="green" icon={<CheckCircle className="h-3 w-3" />} />
\`\`\`
        `}}},argTypes:{label:{control:`text`,description:`Badge label text`},color:{control:`select`,options:[`red`,`orange`,`amber`,`yellow`,`green`,`emerald`,`blue`,`cyan`,`purple`,`slate`],description:`Accent color from the design system`},icon:{control:!1,description:`Optional React node icon displayed before the label`},className:{control:`text`,description:`Additional CSS classes`}}},S={args:{label:`command`,color:`blue`}},C={args:{label:`verification`,color:`green`,icon:(0,m.jsx)(u,{className:`h-3 w-3`})},parameters:{docs:{description:{story:`Badge with a leading icon for additional visual context`}}}},w={render:()=>(0,m.jsx)(`div`,{className:`flex flex-wrap gap-2`,children:[{color:`red`,label:`error`},{color:`orange`,label:`warning`},{color:`amber`,label:`caution`},{color:`yellow`,label:`notice`},{color:`green`,label:`verification`,icon:(0,m.jsx)(u,{className:`h-3 w-3`})},{color:`emerald`,label:`success`},{color:`blue`,label:`command`,icon:(0,m.jsx)(c,{className:`h-3 w-3`})},{color:`cyan`,label:`analysis`},{color:`purple`,label:`action`,icon:(0,m.jsx)(s,{className:`h-3 w-3`})},{color:`slate`,label:`config`,icon:(0,m.jsx)(o,{className:`h-3 w-3`})}].map(({color:e,label:t,icon:n})=>(0,m.jsx)(d,{label:t,color:e,icon:n},e))}),parameters:{docs:{description:{story:`All available accent colors displayed side by side, some with icons`}}}},S.parameters={...S.parameters,docs:{...(h=S.parameters)==null?void 0:h.docs,source:{originalSource:`{
  args: {
    label: "command",
    color: "blue"
  }
}`,...(g=S.parameters)==null||(g=g.docs)==null?void 0:g.source}}},C.parameters={...C.parameters,docs:{...(_=C.parameters)==null?void 0:_.docs,source:{originalSource:`{
  args: {
    label: "verification",
    color: "green",
    icon: <CheckCircle className="h-3 w-3" />
  },
  parameters: {
    docs: {
      description: {
        story: "Badge with a leading icon for additional visual context"
      }
    }
  }
}`,...(v=C.parameters)==null||(v=v.docs)==null?void 0:v.source}}},w.parameters={...w.parameters,docs:{...(y=w.parameters)==null?void 0:y.docs,source:{originalSource:`{
  render: () => {
    const colors: Array<{
      color: AccentColor;
      label: string;
      icon?: React.ReactNode;
    }> = [{
      color: "red",
      label: "error"
    }, {
      color: "orange",
      label: "warning"
    }, {
      color: "amber",
      label: "caution"
    }, {
      color: "yellow",
      label: "notice"
    }, {
      color: "green",
      label: "verification",
      icon: <CheckCircle className="h-3 w-3" />
    }, {
      color: "emerald",
      label: "success"
    }, {
      color: "blue",
      label: "command",
      icon: <Play className="h-3 w-3" />
    }, {
      color: "cyan",
      label: "analysis"
    }, {
      color: "purple",
      label: "action",
      icon: <Zap className="h-3 w-3" />
    }, {
      color: "slate",
      label: "config",
      icon: <Settings className="h-3 w-3" />
    }];
    return <div className="flex flex-wrap gap-2">\r
        {colors.map(({
        color,
        label,
        icon
      }) => <TypeBadge key={color} label={label} color={color} icon={icon} />)}\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "All available accent colors displayed side by side, some with icons"
      }
    }
  }
}`,...(b=w.parameters)==null||(b=b.docs)==null?void 0:b.source}}},T=[`Default`,`WithIcon`,`AllColors`]}))();export{w as AllColors,S as Default,C as WithIcon,T as __namedExportsOrder,x as default};