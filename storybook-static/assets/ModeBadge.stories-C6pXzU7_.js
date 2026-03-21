import{n as e}from"./chunk-BneVvdWh.js";import{t}from"./iframe-YYqiyj2T.js";import{n,t as r}from"./utils-CL72_TDw.js";import{n as i,t as a}from"./colors-CaMkseCC.js";function o({label:e,color:t,count:n,className:a}){let o=i(t);return(0,s.jsxs)(`span`,{className:r(`inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs font-medium`,o.bg,o.text,a),children:[e,n!==void 0&&(0,s.jsx)(`span`,{className:`opacity-70`,children:n})]})}var s,c=e((()=>{n(),a(),s=t(),o.__docgenInfo={description:``,methods:[],displayName:`ModeBadge`,props:{label:{required:!0,tsType:{name:`string`},description:``},color:{required:!0,tsType:{name:`AccentColor`},description:``},count:{required:!1,tsType:{name:`number`},description:``},className:{required:!1,tsType:{name:`string`},description:``}}}})),l,u,d,f,p,m,h,g,_,v,y,b,x,S,C,w,T,E;e((()=>{c(),l=t(),b={title:`UI/Shared/ModeBadge`,component:o,tags:[`autodocs`],parameters:{layout:`centered`,docs:{description:{component:`
Standardized filled badge for displaying mode/category labels. Uses the design
system's accent colors for consistent coloring across the application.

## Usage

\`\`\`tsx
import { ModeBadge } from "./ModeBadge";

<ModeBadge label="SH" color="slate" count={3} />
<ModeBadge label="CHK" color="blue" count={2} />
<ModeBadge label="shell" color="green" />
\`\`\`
        `}}},argTypes:{label:{control:`text`,description:`Badge label text`},color:{control:`select`,options:[`red`,`orange`,`amber`,`yellow`,`green`,`emerald`,`blue`,`cyan`,`purple`,`slate`],description:`Accent color from the design system`},count:{control:{type:`number`,min:0},description:`Optional count displayed after the label`},className:{control:`text`,description:`Additional CSS classes`}}},x={args:{label:`SH`,color:`slate`,count:3}},S={args:{label:`CHK`,color:`blue`,count:2}},C={args:{label:`TST`,color:`purple`,count:1}},w={args:{label:`shell`,color:`green`},parameters:{docs:{description:{story:`Badge without a count, showing just the label`}}}},T={render:()=>(0,l.jsx)(`div`,{className:`flex flex-wrap gap-2`,children:[{color:`red`,label:`RED`},{color:`orange`,label:`ORG`},{color:`amber`,label:`AMB`},{color:`yellow`,label:`YLW`},{color:`green`,label:`GRN`},{color:`emerald`,label:`EMR`},{color:`blue`,label:`BLU`},{color:`cyan`,label:`CYN`},{color:`purple`,label:`PUR`},{color:`slate`,label:`SLT`}].map(({color:e,label:t})=>(0,l.jsx)(o,{label:t,color:e,count:2},e))}),parameters:{docs:{description:{story:`All available accent colors displayed side by side`}}}},x.parameters={...x.parameters,docs:{...(u=x.parameters)==null?void 0:u.docs,source:{originalSource:`{
  args: {
    label: "SH",
    color: "slate",
    count: 3
  }
}`,...(d=x.parameters)==null||(d=d.docs)==null?void 0:d.source}}},S.parameters={...S.parameters,docs:{...(f=S.parameters)==null?void 0:f.docs,source:{originalSource:`{
  args: {
    label: "CHK",
    color: "blue",
    count: 2
  }
}`,...(p=S.parameters)==null||(p=p.docs)==null?void 0:p.source}}},C.parameters={...C.parameters,docs:{...(m=C.parameters)==null?void 0:m.docs,source:{originalSource:`{
  args: {
    label: "TST",
    color: "purple",
    count: 1
  }
}`,...(h=C.parameters)==null||(h=h.docs)==null?void 0:h.source}}},w.parameters={...w.parameters,docs:{...(g=w.parameters)==null?void 0:g.docs,source:{originalSource:`{
  args: {
    label: "shell",
    color: "green"
  },
  parameters: {
    docs: {
      description: {
        story: "Badge without a count, showing just the label"
      }
    }
  }
}`,...(_=w.parameters)==null||(_=_.docs)==null?void 0:_.source}}},T.parameters={...T.parameters,docs:{...(v=T.parameters)==null?void 0:v.docs,source:{originalSource:`{
  render: () => {
    const colors: Array<{
      color: AccentColor;
      label: string;
    }> = [{
      color: "red",
      label: "RED"
    }, {
      color: "orange",
      label: "ORG"
    }, {
      color: "amber",
      label: "AMB"
    }, {
      color: "yellow",
      label: "YLW"
    }, {
      color: "green",
      label: "GRN"
    }, {
      color: "emerald",
      label: "EMR"
    }, {
      color: "blue",
      label: "BLU"
    }, {
      color: "cyan",
      label: "CYN"
    }, {
      color: "purple",
      label: "PUR"
    }, {
      color: "slate",
      label: "SLT"
    }];
    return <div className="flex flex-wrap gap-2">\r
        {colors.map(({
        color,
        label
      }) => <ModeBadge key={color} label={label} color={color} count={2} />)}\r
      </div>;
  },
  parameters: {
    docs: {
      description: {
        story: "All available accent colors displayed side by side"
      }
    }
  }
}`,...(y=T.parameters)==null||(y=y.docs)==null?void 0:y.source}}},E=[`Shell`,`Check`,`Test`,`WithoutCount`,`AllColors`]}))();export{T as AllColors,S as Check,x as Shell,C as Test,w as WithoutCount,E as __namedExportsOrder,b as default};