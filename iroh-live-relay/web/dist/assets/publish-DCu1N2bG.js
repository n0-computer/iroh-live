import{E as S,S as h,e as es,B as ts,a as Ae,b as ss,_ as Ue,M as ae,R as rs,f as De}from"./preload-helper-DcOveGHk.js";function d(e,t,s){function r(o,c){if(o._zod||Object.defineProperty(o,"_zod",{value:{def:c,constr:a,traits:new Set},enumerable:!1}),o._zod.traits.has(e))return;o._zod.traits.add(e),t(o,c);const u=a.prototype,l=Object.keys(u);for(let f=0;f<l.length;f++){const m=l[f];m in o||(o[m]=u[m].bind(o))}}const i=s?.Parent??Object;class n extends i{}Object.defineProperty(n,"name",{value:e});function a(o){var c;const u=s?.Parent?new n:this;r(u,o),(c=u._zod).deferred??(c.deferred=[]);for(const l of u._zod.deferred)l();return u}return Object.defineProperty(a,"init",{value:r}),Object.defineProperty(a,Symbol.hasInstance,{value:o=>s?.Parent&&o instanceof s.Parent?!0:o?._zod?.traits?.has(e)}),Object.defineProperty(a,"name",{value:e}),a}class W extends Error{constructor(){super("Encountered Promise during synchronous parse. Use .parseAsync() instead.")}}class mt extends Error{constructor(t){super(`Encountered unidirectional transform during encode: ${t}`),this.name="ZodEncodeError"}}const wt={};function U(e){return wt}function vt(e){const t=Object.values(e).filter(s=>typeof s=="number");return Object.entries(e).filter(([s,r])=>t.indexOf(+s)===-1).map(([s,r])=>r)}function ze(e,t){return typeof t=="bigint"?t.toString():t}function le(e){return{get value(){{const t=e();return Object.defineProperty(this,"value",{value:t}),t}}}}function Pe(e){return e==null}function Oe(e){const t=e.startsWith("^")?1:0,s=e.endsWith("$")?e.length-1:e.length;return e.slice(t,s)}function is(e,t){const s=(e.toString().split(".")[1]||"").length,r=t.toString();let i=(r.split(".")[1]||"").length;if(i===0&&/\d?e-\d?/.test(r)){const c=r.match(/\d?e-(\d?)/);c?.[1]&&(i=Number.parseInt(c[1]))}const n=s>i?s:i,a=Number.parseInt(e.toFixed(n).replace(".","")),o=Number.parseInt(t.toFixed(n).replace(".",""));return a%o/10**n}const Fe=Symbol("evaluating");function b(e,t,s){let r;Object.defineProperty(e,t,{get(){if(r!==Fe)return r===void 0&&(r=Fe,r=s()),r},set(i){Object.defineProperty(e,t,{value:i})},configurable:!0})}function Z(e,t,s){Object.defineProperty(e,t,{value:s,writable:!0,enumerable:!0,configurable:!0})}function F(...e){const t={};for(const s of e){const r=Object.getOwnPropertyDescriptors(s);Object.assign(t,r)}return Object.defineProperties({},t)}function Ve(e){return JSON.stringify(e)}function ns(e){return e.toLowerCase().trim().replace(/[^\w\s-]/g,"").replace(/[\s_-]+/g,"-").replace(/^-+|-+$/g,"")}const bt="captureStackTrace"in Error?Error.captureStackTrace:(...e)=>{};function ee(e){return typeof e=="object"&&e!==null&&!Array.isArray(e)}const as=le(()=>{if(typeof navigator<"u"&&navigator?.userAgent?.includes("Cloudflare"))return!1;try{const e=Function;return new e(""),!0}catch{return!1}});function X(e){if(ee(e)===!1)return!1;const t=e.constructor;if(t===void 0||typeof t!="function")return!0;const s=t.prototype;return!(ee(s)===!1||Object.prototype.hasOwnProperty.call(s,"isPrototypeOf")===!1)}function gt(e){return X(e)?{...e}:Array.isArray(e)?[...e]:e}const os=new Set(["string","number","symbol"]);function H(e){return e.replace(/[.*+?^${}()|[\]\\]/g,"\\$&")}function V(e,t,s){const r=new e._zod.constr(t??e._zod.def);return(!t||s?.parent)&&(r._zod.parent=e),r}function p(e){const t=e;if(!t)return{};if(typeof t=="string")return{error:()=>t};if(t?.message!==void 0){if(t?.error!==void 0)throw new Error("Cannot specify both `message` and `error` params");t.error=t.message}return delete t.message,typeof t.error=="string"?{...t,error:()=>t.error}:t}function cs(e){return Object.keys(e).filter(t=>e[t]._zod.optin==="optional"&&e[t]._zod.optout==="optional")}const us={safeint:[Number.MIN_SAFE_INTEGER,Number.MAX_SAFE_INTEGER],int32:[-2147483648,2147483647],uint32:[0,4294967295],float32:[-34028234663852886e22,34028234663852886e22],float64:[-Number.MAX_VALUE,Number.MAX_VALUE]};function ds(e,t){const s=e._zod.def,r=s.checks;if(r&&r.length>0)throw new Error(".pick() cannot be used on object schemas containing refinements");const i=F(e._zod.def,{get shape(){const n={};for(const a in t){if(!(a in s.shape))throw new Error(`Unrecognized key: "${a}"`);t[a]&&(n[a]=s.shape[a])}return Z(this,"shape",n),n},checks:[]});return V(e,i)}function ls(e,t){const s=e._zod.def,r=s.checks;if(r&&r.length>0)throw new Error(".omit() cannot be used on object schemas containing refinements");const i=F(e._zod.def,{get shape(){const n={...e._zod.def.shape};for(const a in t){if(!(a in s.shape))throw new Error(`Unrecognized key: "${a}"`);t[a]&&delete n[a]}return Z(this,"shape",n),n},checks:[]});return V(e,i)}function hs(e,t){if(!X(t))throw new Error("Invalid input to extend: expected a plain object");const s=e._zod.def.checks;if(s&&s.length>0){const i=e._zod.def.shape;for(const n in t)if(Object.getOwnPropertyDescriptor(i,n)!==void 0)throw new Error("Cannot overwrite keys on object schemas containing refinements. Use `.safeExtend()` instead.")}const r=F(e._zod.def,{get shape(){const i={...e._zod.def.shape,...t};return Z(this,"shape",i),i}});return V(e,r)}function fs(e,t){if(!X(t))throw new Error("Invalid input to safeExtend: expected a plain object");const s=F(e._zod.def,{get shape(){const r={...e._zod.def.shape,...t};return Z(this,"shape",r),r}});return V(e,s)}function ps(e,t){const s=F(e._zod.def,{get shape(){const r={...e._zod.def.shape,...t._zod.def.shape};return Z(this,"shape",r),r},get catchall(){return t._zod.def.catchall},checks:[]});return V(e,s)}function ms(e,t,s){const r=t._zod.def.checks;if(r&&r.length>0)throw new Error(".partial() cannot be used on object schemas containing refinements");const i=F(t._zod.def,{get shape(){const n=t._zod.def.shape,a={...n};if(s)for(const o in s){if(!(o in n))throw new Error(`Unrecognized key: "${o}"`);s[o]&&(a[o]=e?new e({type:"optional",innerType:n[o]}):n[o])}else for(const o in n)a[o]=e?new e({type:"optional",innerType:n[o]}):n[o];return Z(this,"shape",a),a},checks:[]});return V(t,i)}function ws(e,t,s){const r=F(t._zod.def,{get shape(){const i=t._zod.def.shape,n={...i};if(s)for(const a in s){if(!(a in n))throw new Error(`Unrecognized key: "${a}"`);s[a]&&(n[a]=new e({type:"nonoptional",innerType:i[a]}))}else for(const a in i)n[a]=new e({type:"nonoptional",innerType:i[a]});return Z(this,"shape",n),n}});return V(t,r)}function G(e,t=0){if(e.aborted===!0)return!0;for(let s=t;s<e.issues.length;s++)if(e.issues[s]?.continue!==!0)return!0;return!1}function J(e,t){return t.map(s=>{var r;return(r=s).path??(r.path=[]),s.path.unshift(e),s})}function re(e){return typeof e=="string"?e:e?.message}function D(e,t,s){const r={...e,path:e.path??[]};if(!e.message){const i=re(e.inst?._zod.def?.error?.(e))??re(t?.error?.(e))??re(s.customError?.(e))??re(s.localeError?.(e))??"Invalid input";r.message=i}return delete r.inst,delete r.continue,t?.reportInput||delete r.input,r}function Re(e){return Array.isArray(e)?"array":typeof e=="string"?"string":"unknown"}function te(...e){const[t,s,r]=e;return typeof t=="string"?{message:t,code:"custom",input:s,inst:r}:{...t}}const yt=(e,t)=>{e.name="$ZodError",Object.defineProperty(e,"_zod",{value:e._zod,enumerable:!1}),Object.defineProperty(e,"issues",{value:t,enumerable:!1}),e.message=JSON.stringify(t,ze,2),Object.defineProperty(e,"toString",{value:()=>e.message,enumerable:!1})},_t=d("$ZodError",yt),kt=d("$ZodError",yt,{Parent:Error});function vs(e,t=s=>s.message){const s={},r=[];for(const i of e.issues)i.path.length>0?(s[i.path[0]]=s[i.path[0]]||[],s[i.path[0]].push(t(i))):r.push(t(i));return{formErrors:r,fieldErrors:s}}function bs(e,t=s=>s.message){const s={_errors:[]},r=i=>{for(const n of i.issues)if(n.code==="invalid_union"&&n.errors.length)n.errors.map(a=>r({issues:a}));else if(n.code==="invalid_key")r({issues:n.issues});else if(n.code==="invalid_element")r({issues:n.issues});else if(n.path.length===0)s._errors.push(t(n));else{let a=s,o=0;for(;o<n.path.length;){const c=n.path[o];o===n.path.length-1?(a[c]=a[c]||{_errors:[]},a[c]._errors.push(t(n))):a[c]=a[c]||{_errors:[]},a=a[c],o++}}};return r(e),s}const Te=e=>(t,s,r,i)=>{const n=r?Object.assign(r,{async:!1}):{async:!1},a=t._zod.run({value:s,issues:[]},n);if(a instanceof Promise)throw new W;if(a.issues.length){const o=new(i?.Err??e)(a.issues.map(c=>D(c,n,U())));throw bt(o,i?.callee),o}return a.value},Ne=e=>async(t,s,r,i)=>{const n=r?Object.assign(r,{async:!0}):{async:!0};let a=t._zod.run({value:s,issues:[]},n);if(a instanceof Promise&&(a=await a),a.issues.length){const o=new(i?.Err??e)(a.issues.map(c=>D(c,n,U())));throw bt(o,i?.callee),o}return a.value},he=e=>(t,s,r)=>{const i=r?{...r,async:!1}:{async:!1},n=t._zod.run({value:s,issues:[]},i);if(n instanceof Promise)throw new W;return n.issues.length?{success:!1,error:new(e??_t)(n.issues.map(a=>D(a,i,U())))}:{success:!0,data:n.value}},gs=he(kt),fe=e=>async(t,s,r)=>{const i=r?Object.assign(r,{async:!0}):{async:!0};let n=t._zod.run({value:s,issues:[]},i);return n instanceof Promise&&(n=await n),n.issues.length?{success:!1,error:new e(n.issues.map(a=>D(a,i,U())))}:{success:!0,data:n.value}},ys=fe(kt),_s=e=>(t,s,r)=>{const i=r?Object.assign(r,{direction:"backward"}):{direction:"backward"};return Te(e)(t,s,i)},ks=e=>(t,s,r)=>Te(e)(t,s,r),Is=e=>async(t,s,r)=>{const i=r?Object.assign(r,{direction:"backward"}):{direction:"backward"};return Ne(e)(t,s,i)},zs=e=>async(t,s,r)=>Ne(e)(t,s,r),Es=e=>(t,s,r)=>{const i=r?Object.assign(r,{direction:"backward"}):{direction:"backward"};return he(e)(t,s,i)},Ss=e=>(t,s,r)=>he(e)(t,s,r),As=e=>async(t,s,r)=>{const i=r?Object.assign(r,{direction:"backward"}):{direction:"backward"};return fe(e)(t,s,i)},Ps=e=>async(t,s,r)=>fe(e)(t,s,r),Os=/^[cC][^\s-]{8,}$/,Rs=/^[0-9a-z]+$/,Ts=/^[0-9A-HJKMNP-TV-Za-hjkmnp-tv-z]{26}$/,Ns=/^[0-9a-vA-V]{20}$/,xs=/^[A-Za-z0-9]{27}$/,qs=/^[a-zA-Z0-9_-]{21}$/,$s=/^P(?:(\d+W)|(?!.*W)(?=\d|T\d)(\d+Y)?(\d+M)?(\d+D)?(T(?=\d)(\d+H)?(\d+M)?(\d+([.,]\d+)?S)?)?)$/,Cs=/^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$/,je=e=>e?new RegExp(`^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-${e}[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12})$`):/^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-8][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}|00000000-0000-0000-0000-000000000000|ffffffff-ffff-ffff-ffff-ffffffffffff)$/,Ms=/^(?!\.)(?!.*\.\.)([A-Za-z0-9_'+\-\.]*)[A-Za-z0-9_+-]@([A-Za-z0-9][A-Za-z0-9\-]*\.)+[A-Za-z]{2,}$/,Us="^(\\p{Extended_Pictographic}|\\p{Emoji_Component})+$";function Ds(){return new RegExp(Us,"u")}const Fs=/^(?:(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])$/,Vs=/^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,7}:|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})|:((:[0-9a-fA-F]{1,4}){1,7}|:))$/,js=/^((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\/([0-9]|[1-2][0-9]|3[0-2])$/,Zs=/^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|::|([0-9a-fA-F]{1,4})?::([0-9a-fA-F]{1,4}:?){0,6})\/(12[0-8]|1[01][0-9]|[1-9]?[0-9])$/,Ls=/^$|^(?:[0-9a-zA-Z+/]{4})*(?:(?:[0-9a-zA-Z+/]{2}==)|(?:[0-9a-zA-Z+/]{3}=))?$/,It=/^[A-Za-z0-9_-]*$/,Bs=/^\+[1-9]\d{6,14}$/,zt="(?:(?:\\d\\d[2468][048]|\\d\\d[13579][26]|\\d\\d0[48]|[02468][048]00|[13579][26]00)-02-29|\\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\\d|30)|(?:02)-(?:0[1-9]|1\\d|2[0-8])))",Gs=new RegExp(`^${zt}$`);function Et(e){const t="(?:[01]\\d|2[0-3]):[0-5]\\d";return typeof e.precision=="number"?e.precision===-1?`${t}`:e.precision===0?`${t}:[0-5]\\d`:`${t}:[0-5]\\d\\.\\d{${e.precision}}`:`${t}(?::[0-5]\\d(?:\\.\\d+)?)?`}function Js(e){return new RegExp(`^${Et(e)}$`)}function Ws(e){const t=Et({precision:e.precision}),s=["Z"];e.local&&s.push(""),e.offset&&s.push("([+-](?:[01]\\d|2[0-3]):[0-5]\\d)");const r=`${t}(?:${s.join("|")})`;return new RegExp(`^${zt}T(?:${r})$`)}const Ks=e=>{const t=e?`[\\s\\S]{${e?.minimum??0},${e?.maximum??""}}`:"[\\s\\S]*";return new RegExp(`^${t}$`)},Xs=/^-?\d+$/,St=/^-?\d+(?:\.\d+)?$/,Hs=/^(?:true|false)$/i,Ys=/^[^A-Z]*$/,Qs=/^[^a-z]*$/,R=d("$ZodCheck",(e,t)=>{var s;e._zod??(e._zod={}),e._zod.def=t,(s=e._zod).onattach??(s.onattach=[])}),At={number:"number",bigint:"bigint",object:"date"},Pt=d("$ZodCheckLessThan",(e,t)=>{R.init(e,t);const s=At[typeof t.value];e._zod.onattach.push(r=>{const i=r._zod.bag,n=(t.inclusive?i.maximum:i.exclusiveMaximum)??Number.POSITIVE_INFINITY;t.value<n&&(t.inclusive?i.maximum=t.value:i.exclusiveMaximum=t.value)}),e._zod.check=r=>{(t.inclusive?r.value<=t.value:r.value<t.value)||r.issues.push({origin:s,code:"too_big",maximum:typeof t.value=="object"?t.value.getTime():t.value,input:r.value,inclusive:t.inclusive,inst:e,continue:!t.abort})}}),Ot=d("$ZodCheckGreaterThan",(e,t)=>{R.init(e,t);const s=At[typeof t.value];e._zod.onattach.push(r=>{const i=r._zod.bag,n=(t.inclusive?i.minimum:i.exclusiveMinimum)??Number.NEGATIVE_INFINITY;t.value>n&&(t.inclusive?i.minimum=t.value:i.exclusiveMinimum=t.value)}),e._zod.check=r=>{(t.inclusive?r.value>=t.value:r.value>t.value)||r.issues.push({origin:s,code:"too_small",minimum:typeof t.value=="object"?t.value.getTime():t.value,input:r.value,inclusive:t.inclusive,inst:e,continue:!t.abort})}}),er=d("$ZodCheckMultipleOf",(e,t)=>{R.init(e,t),e._zod.onattach.push(s=>{var r;(r=s._zod.bag).multipleOf??(r.multipleOf=t.value)}),e._zod.check=s=>{if(typeof s.value!=typeof t.value)throw new Error("Cannot mix number and bigint in multiple_of check.");(typeof s.value=="bigint"?s.value%t.value===BigInt(0):is(s.value,t.value)===0)||s.issues.push({origin:typeof s.value,code:"not_multiple_of",divisor:t.value,input:s.value,inst:e,continue:!t.abort})}}),tr=d("$ZodCheckNumberFormat",(e,t)=>{R.init(e,t),t.format=t.format||"float64";const s=t.format?.includes("int"),r=s?"int":"number",[i,n]=us[t.format];e._zod.onattach.push(a=>{const o=a._zod.bag;o.format=t.format,o.minimum=i,o.maximum=n,s&&(o.pattern=Xs)}),e._zod.check=a=>{const o=a.value;if(s){if(!Number.isInteger(o)){a.issues.push({expected:r,format:t.format,code:"invalid_type",continue:!1,input:o,inst:e});return}if(!Number.isSafeInteger(o)){o>0?a.issues.push({input:o,code:"too_big",maximum:Number.MAX_SAFE_INTEGER,note:"Integers must be within the safe integer range.",inst:e,origin:r,inclusive:!0,continue:!t.abort}):a.issues.push({input:o,code:"too_small",minimum:Number.MIN_SAFE_INTEGER,note:"Integers must be within the safe integer range.",inst:e,origin:r,inclusive:!0,continue:!t.abort});return}}o<i&&a.issues.push({origin:"number",input:o,code:"too_small",minimum:i,inclusive:!0,inst:e,continue:!t.abort}),o>n&&a.issues.push({origin:"number",input:o,code:"too_big",maximum:n,inclusive:!0,inst:e,continue:!t.abort})}}),sr=d("$ZodCheckMaxLength",(e,t)=>{var s;R.init(e,t),(s=e._zod.def).when??(s.when=r=>{const i=r.value;return!Pe(i)&&i.length!==void 0}),e._zod.onattach.push(r=>{const i=r._zod.bag.maximum??Number.POSITIVE_INFINITY;t.maximum<i&&(r._zod.bag.maximum=t.maximum)}),e._zod.check=r=>{const i=r.value;if(i.length<=t.maximum)return;const n=Re(i);r.issues.push({origin:n,code:"too_big",maximum:t.maximum,inclusive:!0,input:i,inst:e,continue:!t.abort})}}),rr=d("$ZodCheckMinLength",(e,t)=>{var s;R.init(e,t),(s=e._zod.def).when??(s.when=r=>{const i=r.value;return!Pe(i)&&i.length!==void 0}),e._zod.onattach.push(r=>{const i=r._zod.bag.minimum??Number.NEGATIVE_INFINITY;t.minimum>i&&(r._zod.bag.minimum=t.minimum)}),e._zod.check=r=>{const i=r.value;if(i.length>=t.minimum)return;const n=Re(i);r.issues.push({origin:n,code:"too_small",minimum:t.minimum,inclusive:!0,input:i,inst:e,continue:!t.abort})}}),ir=d("$ZodCheckLengthEquals",(e,t)=>{var s;R.init(e,t),(s=e._zod.def).when??(s.when=r=>{const i=r.value;return!Pe(i)&&i.length!==void 0}),e._zod.onattach.push(r=>{const i=r._zod.bag;i.minimum=t.length,i.maximum=t.length,i.length=t.length}),e._zod.check=r=>{const i=r.value,n=i.length;if(n===t.length)return;const a=Re(i),o=n>t.length;r.issues.push({origin:a,...o?{code:"too_big",maximum:t.length}:{code:"too_small",minimum:t.length},inclusive:!0,exact:!0,input:r.value,inst:e,continue:!t.abort})}}),pe=d("$ZodCheckStringFormat",(e,t)=>{var s,r;R.init(e,t),e._zod.onattach.push(i=>{const n=i._zod.bag;n.format=t.format,t.pattern&&(n.patterns??(n.patterns=new Set),n.patterns.add(t.pattern))}),t.pattern?(s=e._zod).check??(s.check=i=>{t.pattern.lastIndex=0,!t.pattern.test(i.value)&&i.issues.push({origin:"string",code:"invalid_format",format:t.format,input:i.value,...t.pattern?{pattern:t.pattern.toString()}:{},inst:e,continue:!t.abort})}):(r=e._zod).check??(r.check=()=>{})}),nr=d("$ZodCheckRegex",(e,t)=>{pe.init(e,t),e._zod.check=s=>{t.pattern.lastIndex=0,!t.pattern.test(s.value)&&s.issues.push({origin:"string",code:"invalid_format",format:"regex",input:s.value,pattern:t.pattern.toString(),inst:e,continue:!t.abort})}}),ar=d("$ZodCheckLowerCase",(e,t)=>{t.pattern??(t.pattern=Ys),pe.init(e,t)}),or=d("$ZodCheckUpperCase",(e,t)=>{t.pattern??(t.pattern=Qs),pe.init(e,t)}),cr=d("$ZodCheckIncludes",(e,t)=>{R.init(e,t);const s=H(t.includes),r=new RegExp(typeof t.position=="number"?`^.{${t.position}}${s}`:s);t.pattern=r,e._zod.onattach.push(i=>{const n=i._zod.bag;n.patterns??(n.patterns=new Set),n.patterns.add(r)}),e._zod.check=i=>{i.value.includes(t.includes,t.position)||i.issues.push({origin:"string",code:"invalid_format",format:"includes",includes:t.includes,input:i.value,inst:e,continue:!t.abort})}}),ur=d("$ZodCheckStartsWith",(e,t)=>{R.init(e,t);const s=new RegExp(`^${H(t.prefix)}.*`);t.pattern??(t.pattern=s),e._zod.onattach.push(r=>{const i=r._zod.bag;i.patterns??(i.patterns=new Set),i.patterns.add(s)}),e._zod.check=r=>{r.value.startsWith(t.prefix)||r.issues.push({origin:"string",code:"invalid_format",format:"starts_with",prefix:t.prefix,input:r.value,inst:e,continue:!t.abort})}}),dr=d("$ZodCheckEndsWith",(e,t)=>{R.init(e,t);const s=new RegExp(`.*${H(t.suffix)}$`);t.pattern??(t.pattern=s),e._zod.onattach.push(r=>{const i=r._zod.bag;i.patterns??(i.patterns=new Set),i.patterns.add(s)}),e._zod.check=r=>{r.value.endsWith(t.suffix)||r.issues.push({origin:"string",code:"invalid_format",format:"ends_with",suffix:t.suffix,input:r.value,inst:e,continue:!t.abort})}}),lr=d("$ZodCheckOverwrite",(e,t)=>{R.init(e,t),e._zod.check=s=>{s.value=t.tx(s.value)}});class hr{constructor(t=[]){this.content=[],this.indent=0,this&&(this.args=t)}indented(t){this.indent+=1,t(this),this.indent-=1}write(t){if(typeof t=="function"){t(this,{execution:"sync"}),t(this,{execution:"async"});return}const s=t.split(`
`).filter(n=>n),r=Math.min(...s.map(n=>n.length-n.trimStart().length)),i=s.map(n=>n.slice(r)).map(n=>" ".repeat(this.indent*2)+n);for(const n of i)this.content.push(n)}compile(){const t=Function,s=this?.args,r=[...(this?.content??[""]).map(i=>`  ${i}`)];return new t(...s,r.join(`
`))}}const fr={major:4,minor:3,patch:6},_=d("$ZodType",(e,t)=>{var s;e??(e={}),e._zod.def=t,e._zod.bag=e._zod.bag||{},e._zod.version=fr;const r=[...e._zod.def.checks??[]];e._zod.traits.has("$ZodCheck")&&r.unshift(e);for(const i of r)for(const n of i._zod.onattach)n(e);if(r.length===0)(s=e._zod).deferred??(s.deferred=[]),e._zod.deferred?.push(()=>{e._zod.run=e._zod.parse});else{const i=(a,o,c)=>{let u=G(a),l;for(const f of o){if(f._zod.def.when){if(!f._zod.def.when(a))continue}else if(u)continue;const m=a.issues.length,w=f._zod.check(a);if(w instanceof Promise&&c?.async===!1)throw new W;if(l||w instanceof Promise)l=(l??Promise.resolve()).then(async()=>{await w,a.issues.length!==m&&(u||(u=G(a,m)))});else{if(a.issues.length===m)continue;u||(u=G(a,m))}}return l?l.then(()=>a):a},n=(a,o,c)=>{if(G(a))return a.aborted=!0,a;const u=i(o,r,c);if(u instanceof Promise){if(c.async===!1)throw new W;return u.then(l=>e._zod.parse(l,c))}return e._zod.parse(u,c)};e._zod.run=(a,o)=>{if(o.skipChecks)return e._zod.parse(a,o);if(o.direction==="backward"){const u=e._zod.parse({value:a.value,issues:[]},{...o,skipChecks:!0});return u instanceof Promise?u.then(l=>n(l,a,o)):n(u,a,o)}const c=e._zod.parse(a,o);if(c instanceof Promise){if(o.async===!1)throw new W;return c.then(u=>i(u,r,o))}return i(c,r,o)}}b(e,"~standard",()=>({validate:i=>{try{const n=gs(e,i);return n.success?{value:n.data}:{issues:n.error?.issues}}catch{return ys(e,i).then(n=>n.success?{value:n.data}:{issues:n.error?.issues})}},vendor:"zod",version:1}))}),xe=d("$ZodString",(e,t)=>{_.init(e,t),e._zod.pattern=[...e?._zod.bag?.patterns??[]].pop()??Ks(e._zod.bag),e._zod.parse=(s,r)=>{if(t.coerce)try{s.value=String(s.value)}catch{}return typeof s.value=="string"||s.issues.push({expected:"string",code:"invalid_type",input:s.value,inst:e}),s}}),g=d("$ZodStringFormat",(e,t)=>{pe.init(e,t),xe.init(e,t)}),pr=d("$ZodGUID",(e,t)=>{t.pattern??(t.pattern=Cs),g.init(e,t)}),mr=d("$ZodUUID",(e,t)=>{if(t.version){const s={v1:1,v2:2,v3:3,v4:4,v5:5,v6:6,v7:7,v8:8}[t.version];if(s===void 0)throw new Error(`Invalid UUID version: "${t.version}"`);t.pattern??(t.pattern=je(s))}else t.pattern??(t.pattern=je());g.init(e,t)}),wr=d("$ZodEmail",(e,t)=>{t.pattern??(t.pattern=Ms),g.init(e,t)}),vr=d("$ZodURL",(e,t)=>{g.init(e,t),e._zod.check=s=>{try{const r=s.value.trim(),i=new URL(r);t.hostname&&(t.hostname.lastIndex=0,t.hostname.test(i.hostname)||s.issues.push({code:"invalid_format",format:"url",note:"Invalid hostname",pattern:t.hostname.source,input:s.value,inst:e,continue:!t.abort})),t.protocol&&(t.protocol.lastIndex=0,t.protocol.test(i.protocol.endsWith(":")?i.protocol.slice(0,-1):i.protocol)||s.issues.push({code:"invalid_format",format:"url",note:"Invalid protocol",pattern:t.protocol.source,input:s.value,inst:e,continue:!t.abort})),t.normalize?s.value=i.href:s.value=r;return}catch{s.issues.push({code:"invalid_format",format:"url",input:s.value,inst:e,continue:!t.abort})}}}),br=d("$ZodEmoji",(e,t)=>{t.pattern??(t.pattern=Ds()),g.init(e,t)}),gr=d("$ZodNanoID",(e,t)=>{t.pattern??(t.pattern=qs),g.init(e,t)}),yr=d("$ZodCUID",(e,t)=>{t.pattern??(t.pattern=Os),g.init(e,t)}),_r=d("$ZodCUID2",(e,t)=>{t.pattern??(t.pattern=Rs),g.init(e,t)}),kr=d("$ZodULID",(e,t)=>{t.pattern??(t.pattern=Ts),g.init(e,t)}),Ir=d("$ZodXID",(e,t)=>{t.pattern??(t.pattern=Ns),g.init(e,t)}),zr=d("$ZodKSUID",(e,t)=>{t.pattern??(t.pattern=xs),g.init(e,t)}),Er=d("$ZodISODateTime",(e,t)=>{t.pattern??(t.pattern=Ws(t)),g.init(e,t)}),Sr=d("$ZodISODate",(e,t)=>{t.pattern??(t.pattern=Gs),g.init(e,t)}),Ar=d("$ZodISOTime",(e,t)=>{t.pattern??(t.pattern=Js(t)),g.init(e,t)}),Pr=d("$ZodISODuration",(e,t)=>{t.pattern??(t.pattern=$s),g.init(e,t)}),Or=d("$ZodIPv4",(e,t)=>{t.pattern??(t.pattern=Fs),g.init(e,t),e._zod.bag.format="ipv4"}),Rr=d("$ZodIPv6",(e,t)=>{t.pattern??(t.pattern=Vs),g.init(e,t),e._zod.bag.format="ipv6",e._zod.check=s=>{try{new URL(`http://[${s.value}]`)}catch{s.issues.push({code:"invalid_format",format:"ipv6",input:s.value,inst:e,continue:!t.abort})}}}),Tr=d("$ZodCIDRv4",(e,t)=>{t.pattern??(t.pattern=js),g.init(e,t)}),Nr=d("$ZodCIDRv6",(e,t)=>{t.pattern??(t.pattern=Zs),g.init(e,t),e._zod.check=s=>{const r=s.value.split("/");try{if(r.length!==2)throw new Error;const[i,n]=r;if(!n)throw new Error;const a=Number(n);if(`${a}`!==n)throw new Error;if(a<0||a>128)throw new Error;new URL(`http://[${i}]`)}catch{s.issues.push({code:"invalid_format",format:"cidrv6",input:s.value,inst:e,continue:!t.abort})}}});function Rt(e){if(e==="")return!0;if(e.length%4!==0)return!1;try{return atob(e),!0}catch{return!1}}const xr=d("$ZodBase64",(e,t)=>{t.pattern??(t.pattern=Ls),g.init(e,t),e._zod.bag.contentEncoding="base64",e._zod.check=s=>{Rt(s.value)||s.issues.push({code:"invalid_format",format:"base64",input:s.value,inst:e,continue:!t.abort})}});function qr(e){if(!It.test(e))return!1;const t=e.replace(/[-_]/g,r=>r==="-"?"+":"/"),s=t.padEnd(Math.ceil(t.length/4)*4,"=");return Rt(s)}const $r=d("$ZodBase64URL",(e,t)=>{t.pattern??(t.pattern=It),g.init(e,t),e._zod.bag.contentEncoding="base64url",e._zod.check=s=>{qr(s.value)||s.issues.push({code:"invalid_format",format:"base64url",input:s.value,inst:e,continue:!t.abort})}}),Cr=d("$ZodE164",(e,t)=>{t.pattern??(t.pattern=Bs),g.init(e,t)});function Mr(e,t=null){try{const s=e.split(".");if(s.length!==3)return!1;const[r]=s;if(!r)return!1;const i=JSON.parse(atob(r));return!("typ"in i&&i?.typ!=="JWT"||!i.alg||t&&(!("alg"in i)||i.alg!==t))}catch{return!1}}const Ur=d("$ZodJWT",(e,t)=>{g.init(e,t),e._zod.check=s=>{Mr(s.value,t.alg)||s.issues.push({code:"invalid_format",format:"jwt",input:s.value,inst:e,continue:!t.abort})}}),Tt=d("$ZodNumber",(e,t)=>{_.init(e,t),e._zod.pattern=e._zod.bag.pattern??St,e._zod.parse=(s,r)=>{if(t.coerce)try{s.value=Number(s.value)}catch{}const i=s.value;if(typeof i=="number"&&!Number.isNaN(i)&&Number.isFinite(i))return s;const n=typeof i=="number"?Number.isNaN(i)?"NaN":Number.isFinite(i)?void 0:"Infinity":void 0;return s.issues.push({expected:"number",code:"invalid_type",input:i,inst:e,...n?{received:n}:{}}),s}}),Dr=d("$ZodNumberFormat",(e,t)=>{tr.init(e,t),Tt.init(e,t)}),Fr=d("$ZodBoolean",(e,t)=>{_.init(e,t),e._zod.pattern=Hs,e._zod.parse=(s,r)=>{if(t.coerce)try{s.value=!!s.value}catch{}const i=s.value;return typeof i=="boolean"||s.issues.push({expected:"boolean",code:"invalid_type",input:i,inst:e}),s}}),Vr=d("$ZodUnknown",(e,t)=>{_.init(e,t),e._zod.parse=s=>s}),jr=d("$ZodNever",(e,t)=>{_.init(e,t),e._zod.parse=(s,r)=>(s.issues.push({expected:"never",code:"invalid_type",input:s.value,inst:e}),s)});function Ze(e,t,s){e.issues.length&&t.issues.push(...J(s,e.issues)),t.value[s]=e.value}const Zr=d("$ZodArray",(e,t)=>{_.init(e,t),e._zod.parse=(s,r)=>{const i=s.value;if(!Array.isArray(i))return s.issues.push({expected:"array",code:"invalid_type",input:i,inst:e}),s;s.value=Array(i.length);const n=[];for(let a=0;a<i.length;a++){const o=i[a],c=t.element._zod.run({value:o,issues:[]},r);c instanceof Promise?n.push(c.then(u=>Ze(u,s,a))):Ze(c,s,a)}return n.length?Promise.all(n).then(()=>s):s}});function oe(e,t,s,r,i){if(e.issues.length){if(i&&!(s in r))return;t.issues.push(...J(s,e.issues))}e.value===void 0?s in r&&(t.value[s]=void 0):t.value[s]=e.value}function Nt(e){const t=Object.keys(e.shape);for(const r of t)if(!e.shape?.[r]?._zod?.traits?.has("$ZodType"))throw new Error(`Invalid element at key "${r}": expected a Zod schema`);const s=cs(e.shape);return{...e,keys:t,keySet:new Set(t),numKeys:t.length,optionalKeys:new Set(s)}}function xt(e,t,s,r,i,n){const a=[],o=i.keySet,c=i.catchall._zod,u=c.def.type,l=c.optout==="optional";for(const f in t){if(o.has(f))continue;if(u==="never"){a.push(f);continue}const m=c.run({value:t[f],issues:[]},r);m instanceof Promise?e.push(m.then(w=>oe(w,s,f,t,l))):oe(m,s,f,t,l)}return a.length&&s.issues.push({code:"unrecognized_keys",keys:a,input:t,inst:n}),e.length?Promise.all(e).then(()=>s):s}const Lr=d("$ZodObject",(e,t)=>{if(_.init(e,t),!Object.getOwnPropertyDescriptor(t,"shape")?.get){const a=t.shape;Object.defineProperty(t,"shape",{get:()=>{const o={...a};return Object.defineProperty(t,"shape",{value:o}),o}})}const s=le(()=>Nt(t));b(e._zod,"propValues",()=>{const a=t.shape,o={};for(const c in a){const u=a[c]._zod;if(u.values){o[c]??(o[c]=new Set);for(const l of u.values)o[c].add(l)}}return o});const r=ee,i=t.catchall;let n;e._zod.parse=(a,o)=>{n??(n=s.value);const c=a.value;if(!r(c))return a.issues.push({expected:"object",code:"invalid_type",input:c,inst:e}),a;a.value={};const u=[],l=n.shape;for(const f of n.keys){const m=l[f],w=m._zod.optout==="optional",v=m._zod.run({value:c[f],issues:[]},o);v instanceof Promise?u.push(v.then(N=>oe(N,a,f,c,w))):oe(v,a,f,c,w)}return i?xt(u,c,a,o,s.value,e):u.length?Promise.all(u).then(()=>a):a}}),Br=d("$ZodObjectJIT",(e,t)=>{Lr.init(e,t);const s=e._zod.parse,r=le(()=>Nt(t)),i=f=>{const m=new hr(["shape","payload","ctx"]),w=r.value,v=q=>{const O=Ve(q);return`shape[${O}]._zod.run({ value: input[${O}], issues: [] }, ctx)`};m.write("const input = payload.value;");const N=Object.create(null);let B=0;for(const q of w.keys)N[q]=`key_${B++}`;m.write("const newResult = {};");for(const q of w.keys){const O=N[q],x=Ve(q),Qt=f[q]?._zod?.optout==="optional";m.write(`const ${O} = ${v(q)};`),Qt?m.write(`
        if (${O}.issues.length) {
          if (${x} in input) {
            payload.issues = payload.issues.concat(${O}.issues.map(iss => ({
              ...iss,
              path: iss.path ? [${x}, ...iss.path] : [${x}]
            })));
          }
        }
        
        if (${O}.value === undefined) {
          if (${x} in input) {
            newResult[${x}] = undefined;
          }
        } else {
          newResult[${x}] = ${O}.value;
        }
        
      `):m.write(`
        if (${O}.issues.length) {
          payload.issues = payload.issues.concat(${O}.issues.map(iss => ({
            ...iss,
            path: iss.path ? [${x}, ...iss.path] : [${x}]
          })));
        }
        
        if (${O}.value === undefined) {
          if (${x} in input) {
            newResult[${x}] = undefined;
          }
        } else {
          newResult[${x}] = ${O}.value;
        }
        
      `)}m.write("payload.value = newResult;"),m.write("return payload;");const Yt=m.compile();return(q,O)=>Yt(f,q,O)};let n;const a=ee,o=!wt.jitless,c=o&&as.value,u=t.catchall;let l;e._zod.parse=(f,m)=>{l??(l=r.value);const w=f.value;return a(w)?o&&c&&m?.async===!1&&m.jitless!==!0?(n||(n=i(t.shape)),f=n(f,m),u?xt([],w,f,m,l,e):f):s(f,m):(f.issues.push({expected:"object",code:"invalid_type",input:w,inst:e}),f)}});function Le(e,t,s,r){for(const n of e)if(n.issues.length===0)return t.value=n.value,t;const i=e.filter(n=>!G(n));return i.length===1?(t.value=i[0].value,i[0]):(t.issues.push({code:"invalid_union",input:t.value,inst:s,errors:e.map(n=>n.issues.map(a=>D(a,r,U())))}),t)}const qt=d("$ZodUnion",(e,t)=>{_.init(e,t),b(e._zod,"optin",()=>t.options.some(i=>i._zod.optin==="optional")?"optional":void 0),b(e._zod,"optout",()=>t.options.some(i=>i._zod.optout==="optional")?"optional":void 0),b(e._zod,"values",()=>{if(t.options.every(i=>i._zod.values))return new Set(t.options.flatMap(i=>Array.from(i._zod.values)))}),b(e._zod,"pattern",()=>{if(t.options.every(i=>i._zod.pattern)){const i=t.options.map(n=>n._zod.pattern);return new RegExp(`^(${i.map(n=>Oe(n.source)).join("|")})$`)}});const s=t.options.length===1,r=t.options[0]._zod.run;e._zod.parse=(i,n)=>{if(s)return r(i,n);let a=!1;const o=[];for(const c of t.options){const u=c._zod.run({value:i.value,issues:[]},n);if(u instanceof Promise)o.push(u),a=!0;else{if(u.issues.length===0)return u;o.push(u)}}return a?Promise.all(o).then(c=>Le(c,i,e,n)):Le(o,i,e,n)}}),Gr=d("$ZodDiscriminatedUnion",(e,t)=>{t.inclusive=!1,qt.init(e,t);const s=e._zod.parse;b(e._zod,"propValues",()=>{const i={};for(const n of t.options){const a=n._zod.propValues;if(!a||Object.keys(a).length===0)throw new Error(`Invalid discriminated union option at index "${t.options.indexOf(n)}"`);for(const[o,c]of Object.entries(a)){i[o]||(i[o]=new Set);for(const u of c)i[o].add(u)}}return i});const r=le(()=>{const i=t.options,n=new Map;for(const a of i){const o=a._zod.propValues?.[t.discriminator];if(!o||o.size===0)throw new Error(`Invalid discriminated union option at index "${t.options.indexOf(a)}"`);for(const c of o){if(n.has(c))throw new Error(`Duplicate discriminator value "${String(c)}"`);n.set(c,a)}}return n});e._zod.parse=(i,n)=>{const a=i.value;if(!ee(a))return i.issues.push({code:"invalid_type",expected:"object",input:a,inst:e}),i;const o=r.value.get(a?.[t.discriminator]);return o?o._zod.run(i,n):t.unionFallback?s(i,n):(i.issues.push({code:"invalid_union",errors:[],note:"No matching discriminator",discriminator:t.discriminator,input:a,path:[t.discriminator],inst:e}),i)}}),Jr=d("$ZodIntersection",(e,t)=>{_.init(e,t),e._zod.parse=(s,r)=>{const i=s.value,n=t.left._zod.run({value:i,issues:[]},r),a=t.right._zod.run({value:i,issues:[]},r);return n instanceof Promise||a instanceof Promise?Promise.all([n,a]).then(([o,c])=>Be(s,o,c)):Be(s,n,a)}});function Ee(e,t){if(e===t)return{valid:!0,data:e};if(e instanceof Date&&t instanceof Date&&+e==+t)return{valid:!0,data:e};if(X(e)&&X(t)){const s=Object.keys(t),r=Object.keys(e).filter(n=>s.indexOf(n)!==-1),i={...e,...t};for(const n of r){const a=Ee(e[n],t[n]);if(!a.valid)return{valid:!1,mergeErrorPath:[n,...a.mergeErrorPath]};i[n]=a.data}return{valid:!0,data:i}}if(Array.isArray(e)&&Array.isArray(t)){if(e.length!==t.length)return{valid:!1,mergeErrorPath:[]};const s=[];for(let r=0;r<e.length;r++){const i=e[r],n=t[r],a=Ee(i,n);if(!a.valid)return{valid:!1,mergeErrorPath:[r,...a.mergeErrorPath]};s.push(a.data)}return{valid:!0,data:s}}return{valid:!1,mergeErrorPath:[]}}function Be(e,t,s){const r=new Map;let i;for(const o of t.issues)if(o.code==="unrecognized_keys"){i??(i=o);for(const c of o.keys)r.has(c)||r.set(c,{}),r.get(c).l=!0}else e.issues.push(o);for(const o of s.issues)if(o.code==="unrecognized_keys")for(const c of o.keys)r.has(c)||r.set(c,{}),r.get(c).r=!0;else e.issues.push(o);const n=[...r].filter(([,o])=>o.l&&o.r).map(([o])=>o);if(n.length&&i&&e.issues.push({...i,keys:n}),G(e))return e;const a=Ee(t.value,s.value);if(!a.valid)throw new Error(`Unmergable intersection. Error path: ${JSON.stringify(a.mergeErrorPath)}`);return e.value=a.data,e}const Wr=d("$ZodRecord",(e,t)=>{_.init(e,t),e._zod.parse=(s,r)=>{const i=s.value;if(!X(i))return s.issues.push({expected:"record",code:"invalid_type",input:i,inst:e}),s;const n=[],a=t.keyType._zod.values;if(a){s.value={};const o=new Set;for(const u of a)if(typeof u=="string"||typeof u=="number"||typeof u=="symbol"){o.add(typeof u=="number"?u.toString():u);const l=t.valueType._zod.run({value:i[u],issues:[]},r);l instanceof Promise?n.push(l.then(f=>{f.issues.length&&s.issues.push(...J(u,f.issues)),s.value[u]=f.value})):(l.issues.length&&s.issues.push(...J(u,l.issues)),s.value[u]=l.value)}let c;for(const u in i)o.has(u)||(c=c??[],c.push(u));c&&c.length>0&&s.issues.push({code:"unrecognized_keys",input:i,inst:e,keys:c})}else{s.value={};for(const o of Reflect.ownKeys(i)){if(o==="__proto__")continue;let c=t.keyType._zod.run({value:o,issues:[]},r);if(c instanceof Promise)throw new Error("Async schemas not supported in object keys currently");if(typeof o=="string"&&St.test(o)&&c.issues.length){const l=t.keyType._zod.run({value:Number(o),issues:[]},r);if(l instanceof Promise)throw new Error("Async schemas not supported in object keys currently");l.issues.length===0&&(c=l)}if(c.issues.length){t.mode==="loose"?s.value[o]=i[o]:s.issues.push({code:"invalid_key",origin:"record",issues:c.issues.map(l=>D(l,r,U())),input:o,path:[o],inst:e});continue}const u=t.valueType._zod.run({value:i[o],issues:[]},r);u instanceof Promise?n.push(u.then(l=>{l.issues.length&&s.issues.push(...J(o,l.issues)),s.value[c.value]=l.value})):(u.issues.length&&s.issues.push(...J(o,u.issues)),s.value[c.value]=u.value)}}return n.length?Promise.all(n).then(()=>s):s}}),Kr=d("$ZodEnum",(e,t)=>{_.init(e,t);const s=vt(t.entries),r=new Set(s);e._zod.values=r,e._zod.pattern=new RegExp(`^(${s.filter(i=>os.has(typeof i)).map(i=>typeof i=="string"?H(i):i.toString()).join("|")})$`),e._zod.parse=(i,n)=>{const a=i.value;return r.has(a)||i.issues.push({code:"invalid_value",values:s,input:a,inst:e}),i}}),Xr=d("$ZodLiteral",(e,t)=>{if(_.init(e,t),t.values.length===0)throw new Error("Cannot create literal schema with no valid values");const s=new Set(t.values);e._zod.values=s,e._zod.pattern=new RegExp(`^(${t.values.map(r=>typeof r=="string"?H(r):r?H(r.toString()):String(r)).join("|")})$`),e._zod.parse=(r,i)=>{const n=r.value;return s.has(n)||r.issues.push({code:"invalid_value",values:t.values,input:n,inst:e}),r}}),Hr=d("$ZodTransform",(e,t)=>{_.init(e,t),e._zod.parse=(s,r)=>{if(r.direction==="backward")throw new mt(e.constructor.name);const i=t.transform(s.value,s);if(r.async)return(i instanceof Promise?i:Promise.resolve(i)).then(n=>(s.value=n,s));if(i instanceof Promise)throw new W;return s.value=i,s}});function Ge(e,t){return e.issues.length&&t===void 0?{issues:[],value:void 0}:e}const $t=d("$ZodOptional",(e,t)=>{_.init(e,t),e._zod.optin="optional",e._zod.optout="optional",b(e._zod,"values",()=>t.innerType._zod.values?new Set([...t.innerType._zod.values,void 0]):void 0),b(e._zod,"pattern",()=>{const s=t.innerType._zod.pattern;return s?new RegExp(`^(${Oe(s.source)})?$`):void 0}),e._zod.parse=(s,r)=>{if(t.innerType._zod.optin==="optional"){const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(n=>Ge(n,s.value)):Ge(i,s.value)}return s.value===void 0?s:t.innerType._zod.run(s,r)}}),Yr=d("$ZodExactOptional",(e,t)=>{$t.init(e,t),b(e._zod,"values",()=>t.innerType._zod.values),b(e._zod,"pattern",()=>t.innerType._zod.pattern),e._zod.parse=(s,r)=>t.innerType._zod.run(s,r)}),Qr=d("$ZodNullable",(e,t)=>{_.init(e,t),b(e._zod,"optin",()=>t.innerType._zod.optin),b(e._zod,"optout",()=>t.innerType._zod.optout),b(e._zod,"pattern",()=>{const s=t.innerType._zod.pattern;return s?new RegExp(`^(${Oe(s.source)}|null)$`):void 0}),b(e._zod,"values",()=>t.innerType._zod.values?new Set([...t.innerType._zod.values,null]):void 0),e._zod.parse=(s,r)=>s.value===null?s:t.innerType._zod.run(s,r)}),ei=d("$ZodDefault",(e,t)=>{_.init(e,t),e._zod.optin="optional",b(e._zod,"values",()=>t.innerType._zod.values),e._zod.parse=(s,r)=>{if(r.direction==="backward")return t.innerType._zod.run(s,r);if(s.value===void 0)return s.value=t.defaultValue,s;const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(n=>Je(n,t)):Je(i,t)}});function Je(e,t){return e.value===void 0&&(e.value=t.defaultValue),e}const ti=d("$ZodPrefault",(e,t)=>{_.init(e,t),e._zod.optin="optional",b(e._zod,"values",()=>t.innerType._zod.values),e._zod.parse=(s,r)=>(r.direction==="backward"||s.value===void 0&&(s.value=t.defaultValue),t.innerType._zod.run(s,r))}),si=d("$ZodNonOptional",(e,t)=>{_.init(e,t),b(e._zod,"values",()=>{const s=t.innerType._zod.values;return s?new Set([...s].filter(r=>r!==void 0)):void 0}),e._zod.parse=(s,r)=>{const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(n=>We(n,e)):We(i,e)}});function We(e,t){return!e.issues.length&&e.value===void 0&&e.issues.push({code:"invalid_type",expected:"nonoptional",input:e.value,inst:t}),e}const ri=d("$ZodCatch",(e,t)=>{_.init(e,t),b(e._zod,"optin",()=>t.innerType._zod.optin),b(e._zod,"optout",()=>t.innerType._zod.optout),b(e._zod,"values",()=>t.innerType._zod.values),e._zod.parse=(s,r)=>{if(r.direction==="backward")return t.innerType._zod.run(s,r);const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(n=>(s.value=n.value,n.issues.length&&(s.value=t.catchValue({...s,error:{issues:n.issues.map(a=>D(a,r,U()))},input:s.value}),s.issues=[]),s)):(s.value=i.value,i.issues.length&&(s.value=t.catchValue({...s,error:{issues:i.issues.map(n=>D(n,r,U()))},input:s.value}),s.issues=[]),s)}}),ii=d("$ZodPipe",(e,t)=>{_.init(e,t),b(e._zod,"values",()=>t.in._zod.values),b(e._zod,"optin",()=>t.in._zod.optin),b(e._zod,"optout",()=>t.out._zod.optout),b(e._zod,"propValues",()=>t.in._zod.propValues),e._zod.parse=(s,r)=>{if(r.direction==="backward"){const n=t.out._zod.run(s,r);return n instanceof Promise?n.then(a=>ie(a,t.in,r)):ie(n,t.in,r)}const i=t.in._zod.run(s,r);return i instanceof Promise?i.then(n=>ie(n,t.out,r)):ie(i,t.out,r)}});function ie(e,t,s){return e.issues.length?(e.aborted=!0,e):t._zod.run({value:e.value,issues:e.issues},s)}const ni=d("$ZodReadonly",(e,t)=>{_.init(e,t),b(e._zod,"propValues",()=>t.innerType._zod.propValues),b(e._zod,"values",()=>t.innerType._zod.values),b(e._zod,"optin",()=>t.innerType?._zod?.optin),b(e._zod,"optout",()=>t.innerType?._zod?.optout),e._zod.parse=(s,r)=>{if(r.direction==="backward")return t.innerType._zod.run(s,r);const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(Ke):Ke(i)}});function Ke(e){return e.value=Object.freeze(e.value),e}const ai=d("$ZodCustom",(e,t)=>{R.init(e,t),_.init(e,t),e._zod.parse=(s,r)=>s,e._zod.check=s=>{const r=s.value,i=t.fn(r);if(i instanceof Promise)return i.then(n=>Xe(n,s,r,e));Xe(i,s,r,e)}});function Xe(e,t,s,r){if(!e){const i={code:"custom",input:s,inst:r,path:[...r._zod.def.path??[]],continue:!r._zod.def.abort};r._zod.def.params&&(i.params=r._zod.def.params),t.issues.push(te(i))}}var He;class oi{constructor(){this._map=new WeakMap,this._idmap=new Map}add(t,...s){const r=s[0];return this._map.set(t,r),r&&typeof r=="object"&&"id"in r&&this._idmap.set(r.id,t),this}clear(){return this._map=new WeakMap,this._idmap=new Map,this}remove(t){const s=this._map.get(t);return s&&typeof s=="object"&&"id"in s&&this._idmap.delete(s.id),this._map.delete(t),this}get(t){const s=t._zod.parent;if(s){const r={...this.get(s)??{}};delete r.id;const i={...r,...this._map.get(t)};return Object.keys(i).length?i:void 0}return this._map.get(t)}has(t){return this._map.has(t)}}function ci(){return new oi}(He=globalThis).__zod_globalRegistry??(He.__zod_globalRegistry=ci());const Q=globalThis.__zod_globalRegistry;function ui(e,t){return new e({type:"string",...p(t)})}function di(e,t){return new e({type:"string",format:"email",check:"string_format",abort:!1,...p(t)})}function Ye(e,t){return new e({type:"string",format:"guid",check:"string_format",abort:!1,...p(t)})}function li(e,t){return new e({type:"string",format:"uuid",check:"string_format",abort:!1,...p(t)})}function hi(e,t){return new e({type:"string",format:"uuid",check:"string_format",abort:!1,version:"v4",...p(t)})}function fi(e,t){return new e({type:"string",format:"uuid",check:"string_format",abort:!1,version:"v6",...p(t)})}function pi(e,t){return new e({type:"string",format:"uuid",check:"string_format",abort:!1,version:"v7",...p(t)})}function mi(e,t){return new e({type:"string",format:"url",check:"string_format",abort:!1,...p(t)})}function wi(e,t){return new e({type:"string",format:"emoji",check:"string_format",abort:!1,...p(t)})}function vi(e,t){return new e({type:"string",format:"nanoid",check:"string_format",abort:!1,...p(t)})}function bi(e,t){return new e({type:"string",format:"cuid",check:"string_format",abort:!1,...p(t)})}function gi(e,t){return new e({type:"string",format:"cuid2",check:"string_format",abort:!1,...p(t)})}function yi(e,t){return new e({type:"string",format:"ulid",check:"string_format",abort:!1,...p(t)})}function _i(e,t){return new e({type:"string",format:"xid",check:"string_format",abort:!1,...p(t)})}function ki(e,t){return new e({type:"string",format:"ksuid",check:"string_format",abort:!1,...p(t)})}function Ii(e,t){return new e({type:"string",format:"ipv4",check:"string_format",abort:!1,...p(t)})}function zi(e,t){return new e({type:"string",format:"ipv6",check:"string_format",abort:!1,...p(t)})}function Ei(e,t){return new e({type:"string",format:"cidrv4",check:"string_format",abort:!1,...p(t)})}function Si(e,t){return new e({type:"string",format:"cidrv6",check:"string_format",abort:!1,...p(t)})}function Ai(e,t){return new e({type:"string",format:"base64",check:"string_format",abort:!1,...p(t)})}function Pi(e,t){return new e({type:"string",format:"base64url",check:"string_format",abort:!1,...p(t)})}function Oi(e,t){return new e({type:"string",format:"e164",check:"string_format",abort:!1,...p(t)})}function Ri(e,t){return new e({type:"string",format:"jwt",check:"string_format",abort:!1,...p(t)})}function Ti(e,t){return new e({type:"string",format:"datetime",check:"string_format",offset:!1,local:!1,precision:null,...p(t)})}function Ni(e,t){return new e({type:"string",format:"date",check:"string_format",...p(t)})}function xi(e,t){return new e({type:"string",format:"time",check:"string_format",precision:null,...p(t)})}function qi(e,t){return new e({type:"string",format:"duration",check:"string_format",...p(t)})}function $i(e,t){return new e({type:"number",checks:[],...p(t)})}function Ci(e,t){return new e({type:"number",check:"number_format",abort:!1,format:"safeint",...p(t)})}function Mi(e,t){return new e({type:"boolean",...p(t)})}function Ui(e){return new e({type:"unknown"})}function Di(e,t){return new e({type:"never",...p(t)})}function Qe(e,t){return new Pt({check:"less_than",...p(t),value:e,inclusive:!1})}function ye(e,t){return new Pt({check:"less_than",...p(t),value:e,inclusive:!0})}function et(e,t){return new Ot({check:"greater_than",...p(t),value:e,inclusive:!1})}function _e(e,t){return new Ot({check:"greater_than",...p(t),value:e,inclusive:!0})}function tt(e,t){return new er({check:"multiple_of",...p(t),value:e})}function Ct(e,t){return new sr({check:"max_length",...p(t),maximum:e})}function ce(e,t){return new rr({check:"min_length",...p(t),minimum:e})}function Mt(e,t){return new ir({check:"length_equals",...p(t),length:e})}function Fi(e,t){return new nr({check:"string_format",format:"regex",...p(t),pattern:e})}function Vi(e){return new ar({check:"string_format",format:"lowercase",...p(e)})}function ji(e){return new or({check:"string_format",format:"uppercase",...p(e)})}function Zi(e,t){return new cr({check:"string_format",format:"includes",...p(t),includes:e})}function Li(e,t){return new ur({check:"string_format",format:"starts_with",...p(t),prefix:e})}function Bi(e,t){return new dr({check:"string_format",format:"ends_with",...p(t),suffix:e})}function Y(e){return new lr({check:"overwrite",tx:e})}function Gi(e){return Y(t=>t.normalize(e))}function Ji(){return Y(e=>e.trim())}function Wi(){return Y(e=>e.toLowerCase())}function Ki(){return Y(e=>e.toUpperCase())}function Xi(){return Y(e=>ns(e))}function Hi(e,t,s){return new e({type:"array",element:t,...p(s)})}function Yi(e,t,s){return new e({type:"custom",check:"custom",fn:t,...p(s)})}function Qi(e){const t=en(s=>(s.addIssue=r=>{if(typeof r=="string")s.issues.push(te(r,s.value,t._zod.def));else{const i=r;i.fatal&&(i.continue=!1),i.code??(i.code="custom"),i.input??(i.input=s.value),i.inst??(i.inst=t),i.continue??(i.continue=!t._zod.def.abort),s.issues.push(te(i))}},e(s.value,s)));return t}function en(e,t){const s=new R({check:"custom",...p(t)});return s._zod.check=e,s}function Ut(e){let t=e?.target??"draft-2020-12";return t==="draft-4"&&(t="draft-04"),t==="draft-7"&&(t="draft-07"),{processors:e.processors??{},metadataRegistry:e?.metadata??Q,target:t,unrepresentable:e?.unrepresentable??"throw",override:e?.override??(()=>{}),io:e?.io??"output",counter:0,seen:new Map,cycles:e?.cycles??"ref",reused:e?.reused??"inline",external:e?.external??void 0}}function E(e,t,s={path:[],schemaPath:[]}){var r;const i=e._zod.def,n=t.seen.get(e);if(n)return n.count++,s.schemaPath.includes(e)&&(n.cycle=s.path),n.schema;const a={schema:{},count:1,cycle:void 0,path:s.path};t.seen.set(e,a);const o=e._zod.toJSONSchema?.();if(o)a.schema=o;else{const u={...s,schemaPath:[...s.schemaPath,e],path:s.path};if(e._zod.processJSONSchema)e._zod.processJSONSchema(t,a.schema,u);else{const f=a.schema,m=t.processors[i.type];if(!m)throw new Error(`[toJSONSchema]: Non-representable type encountered: ${i.type}`);m(e,t,f,u)}const l=e._zod.parent;l&&(a.ref||(a.ref=l),E(l,t,u),t.seen.get(l).isParent=!0)}const c=t.metadataRegistry.get(e);return c&&Object.assign(a.schema,c),t.io==="input"&&A(e)&&(delete a.schema.examples,delete a.schema.default),t.io==="input"&&a.schema._prefault&&((r=a.schema).default??(r.default=a.schema._prefault)),delete a.schema._prefault,t.seen.get(e).schema}function Dt(e,t){const s=e.seen.get(t);if(!s)throw new Error("Unprocessed schema. This is a bug in Zod.");const r=new Map;for(const a of e.seen.entries()){const o=e.metadataRegistry.get(a[0])?.id;if(o){const c=r.get(o);if(c&&c!==a[0])throw new Error(`Duplicate schema id "${o}" detected during JSON Schema conversion. Two different schemas cannot share the same id when converted together.`);r.set(o,a[0])}}const i=a=>{const o=e.target==="draft-2020-12"?"$defs":"definitions";if(e.external){const l=e.external.registry.get(a[0])?.id,f=e.external.uri??(w=>w);if(l)return{ref:f(l)};const m=a[1].defId??a[1].schema.id??`schema${e.counter++}`;return a[1].defId=m,{defId:m,ref:`${f("__shared")}#/${o}/${m}`}}if(a[1]===s)return{ref:"#"};const c=`#/${o}/`,u=a[1].schema.id??`__schema${e.counter++}`;return{defId:u,ref:c+u}},n=a=>{if(a[1].schema.$ref)return;const o=a[1],{ref:c,defId:u}=i(a);o.def={...o.schema},u&&(o.defId=u);const l=o.schema;for(const f in l)delete l[f];l.$ref=c};if(e.cycles==="throw")for(const a of e.seen.entries()){const o=a[1];if(o.cycle)throw new Error(`Cycle detected: #/${o.cycle?.join("/")}/<root>

Set the \`cycles\` parameter to \`"ref"\` to resolve cyclical schemas with defs.`)}for(const a of e.seen.entries()){const o=a[1];if(t===a[0]){n(a);continue}if(e.external){const c=e.external.registry.get(a[0])?.id;if(t!==a[0]&&c){n(a);continue}}if(e.metadataRegistry.get(a[0])?.id){n(a);continue}if(o.cycle){n(a);continue}if(o.count>1&&e.reused==="ref"){n(a);continue}}}function Ft(e,t){const s=e.seen.get(t);if(!s)throw new Error("Unprocessed schema. This is a bug in Zod.");const r=a=>{const o=e.seen.get(a);if(o.ref===null)return;const c=o.def??o.schema,u={...c},l=o.ref;if(o.ref=null,l){r(l);const m=e.seen.get(l),w=m.schema;if(w.$ref&&(e.target==="draft-07"||e.target==="draft-04"||e.target==="openapi-3.0")?(c.allOf=c.allOf??[],c.allOf.push(w)):Object.assign(c,w),Object.assign(c,u),a._zod.parent===l)for(const v in c)v==="$ref"||v==="allOf"||v in u||delete c[v];if(w.$ref&&m.def)for(const v in c)v==="$ref"||v==="allOf"||v in m.def&&JSON.stringify(c[v])===JSON.stringify(m.def[v])&&delete c[v]}const f=a._zod.parent;if(f&&f!==l){r(f);const m=e.seen.get(f);if(m?.schema.$ref&&(c.$ref=m.schema.$ref,m.def))for(const w in c)w==="$ref"||w==="allOf"||w in m.def&&JSON.stringify(c[w])===JSON.stringify(m.def[w])&&delete c[w]}e.override({zodSchema:a,jsonSchema:c,path:o.path??[]})};for(const a of[...e.seen.entries()].reverse())r(a[0]);const i={};if(e.target==="draft-2020-12"?i.$schema="https://json-schema.org/draft/2020-12/schema":e.target==="draft-07"?i.$schema="http://json-schema.org/draft-07/schema#":e.target==="draft-04"?i.$schema="http://json-schema.org/draft-04/schema#":e.target,e.external?.uri){const a=e.external.registry.get(t)?.id;if(!a)throw new Error("Schema is missing an `id` property");i.$id=e.external.uri(a)}Object.assign(i,s.def??s.schema);const n=e.external?.defs??{};for(const a of e.seen.entries()){const o=a[1];o.def&&o.defId&&(n[o.defId]=o.def)}e.external||Object.keys(n).length>0&&(e.target==="draft-2020-12"?i.$defs=n:i.definitions=n);try{const a=JSON.parse(JSON.stringify(i));return Object.defineProperty(a,"~standard",{value:{...t["~standard"],jsonSchema:{input:ue(t,"input",e.processors),output:ue(t,"output",e.processors)}},enumerable:!1,writable:!1}),a}catch{throw new Error("Error converting schema to JSON.")}}function A(e,t){const s=t??{seen:new Set};if(s.seen.has(e))return!1;s.seen.add(e);const r=e._zod.def;if(r.type==="transform")return!0;if(r.type==="array")return A(r.element,s);if(r.type==="set")return A(r.valueType,s);if(r.type==="lazy")return A(r.getter(),s);if(r.type==="promise"||r.type==="optional"||r.type==="nonoptional"||r.type==="nullable"||r.type==="readonly"||r.type==="default"||r.type==="prefault")return A(r.innerType,s);if(r.type==="intersection")return A(r.left,s)||A(r.right,s);if(r.type==="record"||r.type==="map")return A(r.keyType,s)||A(r.valueType,s);if(r.type==="pipe")return A(r.in,s)||A(r.out,s);if(r.type==="object"){for(const i in r.shape)if(A(r.shape[i],s))return!0;return!1}if(r.type==="union"){for(const i of r.options)if(A(i,s))return!0;return!1}if(r.type==="tuple"){for(const i of r.items)if(A(i,s))return!0;return!!(r.rest&&A(r.rest,s))}return!1}const tn=(e,t={})=>s=>{const r=Ut({...s,processors:t});return E(e,r),Dt(r,e),Ft(r,e)},ue=(e,t,s={})=>r=>{const{libraryOptions:i,target:n}=r??{},a=Ut({...i??{},target:n,io:t,processors:s});return E(e,a),Dt(a,e),Ft(a,e)},sn={guid:"uuid",url:"uri",datetime:"date-time",json_string:"json-string",regex:""},rn=(e,t,s,r)=>{const i=s;i.type="string";const{minimum:n,maximum:a,format:o,patterns:c,contentEncoding:u}=e._zod.bag;if(typeof n=="number"&&(i.minLength=n),typeof a=="number"&&(i.maxLength=a),o&&(i.format=sn[o]??o,i.format===""&&delete i.format,o==="time"&&delete i.format),u&&(i.contentEncoding=u),c&&c.size>0){const l=[...c];l.length===1?i.pattern=l[0].source:l.length>1&&(i.allOf=[...l.map(f=>({...t.target==="draft-07"||t.target==="draft-04"||t.target==="openapi-3.0"?{type:"string"}:{},pattern:f.source}))])}},nn=(e,t,s,r)=>{const i=s,{minimum:n,maximum:a,format:o,multipleOf:c,exclusiveMaximum:u,exclusiveMinimum:l}=e._zod.bag;typeof o=="string"&&o.includes("int")?i.type="integer":i.type="number",typeof l=="number"&&(t.target==="draft-04"||t.target==="openapi-3.0"?(i.minimum=l,i.exclusiveMinimum=!0):i.exclusiveMinimum=l),typeof n=="number"&&(i.minimum=n,typeof l=="number"&&t.target!=="draft-04"&&(l>=n?delete i.minimum:delete i.exclusiveMinimum)),typeof u=="number"&&(t.target==="draft-04"||t.target==="openapi-3.0"?(i.maximum=u,i.exclusiveMaximum=!0):i.exclusiveMaximum=u),typeof a=="number"&&(i.maximum=a,typeof u=="number"&&t.target!=="draft-04"&&(u<=a?delete i.maximum:delete i.exclusiveMaximum)),typeof c=="number"&&(i.multipleOf=c)},an=(e,t,s,r)=>{s.type="boolean"},on=(e,t,s,r)=>{s.not={}},cn=(e,t,s,r)=>{},un=(e,t,s,r)=>{const i=e._zod.def,n=vt(i.entries);n.every(a=>typeof a=="number")&&(s.type="number"),n.every(a=>typeof a=="string")&&(s.type="string"),s.enum=n},dn=(e,t,s,r)=>{const i=e._zod.def,n=[];for(const a of i.values)if(a===void 0){if(t.unrepresentable==="throw")throw new Error("Literal `undefined` cannot be represented in JSON Schema")}else if(typeof a=="bigint"){if(t.unrepresentable==="throw")throw new Error("BigInt literals cannot be represented in JSON Schema");n.push(Number(a))}else n.push(a);if(n.length!==0)if(n.length===1){const a=n[0];s.type=a===null?"null":typeof a,t.target==="draft-04"||t.target==="openapi-3.0"?s.enum=[a]:s.const=a}else n.every(a=>typeof a=="number")&&(s.type="number"),n.every(a=>typeof a=="string")&&(s.type="string"),n.every(a=>typeof a=="boolean")&&(s.type="boolean"),n.every(a=>a===null)&&(s.type="null"),s.enum=n},ln=(e,t,s,r)=>{if(t.unrepresentable==="throw")throw new Error("Custom types cannot be represented in JSON Schema")},hn=(e,t,s,r)=>{if(t.unrepresentable==="throw")throw new Error("Transforms cannot be represented in JSON Schema")},fn=(e,t,s,r)=>{const i=s,n=e._zod.def,{minimum:a,maximum:o}=e._zod.bag;typeof a=="number"&&(i.minItems=a),typeof o=="number"&&(i.maxItems=o),i.type="array",i.items=E(n.element,t,{...r,path:[...r.path,"items"]})},pn=(e,t,s,r)=>{const i=s,n=e._zod.def;i.type="object",i.properties={};const a=n.shape;for(const u in a)i.properties[u]=E(a[u],t,{...r,path:[...r.path,"properties",u]});const o=new Set(Object.keys(a)),c=new Set([...o].filter(u=>{const l=n.shape[u]._zod;return t.io==="input"?l.optin===void 0:l.optout===void 0}));c.size>0&&(i.required=Array.from(c)),n.catchall?._zod.def.type==="never"?i.additionalProperties=!1:n.catchall?n.catchall&&(i.additionalProperties=E(n.catchall,t,{...r,path:[...r.path,"additionalProperties"]})):t.io==="output"&&(i.additionalProperties=!1)},mn=(e,t,s,r)=>{const i=e._zod.def,n=i.inclusive===!1,a=i.options.map((o,c)=>E(o,t,{...r,path:[...r.path,n?"oneOf":"anyOf",c]}));n?s.oneOf=a:s.anyOf=a},wn=(e,t,s,r)=>{const i=e._zod.def,n=E(i.left,t,{...r,path:[...r.path,"allOf",0]}),a=E(i.right,t,{...r,path:[...r.path,"allOf",1]}),o=u=>"allOf"in u&&Object.keys(u).length===1,c=[...o(n)?n.allOf:[n],...o(a)?a.allOf:[a]];s.allOf=c},vn=(e,t,s,r)=>{const i=s,n=e._zod.def;i.type="object";const a=n.keyType,o=a._zod.bag?.patterns;if(n.mode==="loose"&&o&&o.size>0){const u=E(n.valueType,t,{...r,path:[...r.path,"patternProperties","*"]});i.patternProperties={};for(const l of o)i.patternProperties[l.source]=u}else(t.target==="draft-07"||t.target==="draft-2020-12")&&(i.propertyNames=E(n.keyType,t,{...r,path:[...r.path,"propertyNames"]})),i.additionalProperties=E(n.valueType,t,{...r,path:[...r.path,"additionalProperties"]});const c=a._zod.values;if(c){const u=[...c].filter(l=>typeof l=="string"||typeof l=="number");u.length>0&&(i.required=u)}},bn=(e,t,s,r)=>{const i=e._zod.def,n=E(i.innerType,t,r),a=t.seen.get(e);t.target==="openapi-3.0"?(a.ref=i.innerType,s.nullable=!0):s.anyOf=[n,{type:"null"}]},gn=(e,t,s,r)=>{const i=e._zod.def;E(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType},yn=(e,t,s,r)=>{const i=e._zod.def;E(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType,s.default=JSON.parse(JSON.stringify(i.defaultValue))},_n=(e,t,s,r)=>{const i=e._zod.def;E(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType,t.io==="input"&&(s._prefault=JSON.parse(JSON.stringify(i.defaultValue)))},kn=(e,t,s,r)=>{const i=e._zod.def;E(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType;let a;try{a=i.catchValue(void 0)}catch{throw new Error("Dynamic catch values are not supported in JSON Schema")}s.default=a},In=(e,t,s,r)=>{const i=e._zod.def,n=t.io==="input"?i.in._zod.def.type==="transform"?i.out:i.in:i.out;E(n,t,r);const a=t.seen.get(e);a.ref=n},zn=(e,t,s,r)=>{const i=e._zod.def;E(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType,s.readOnly=!0},Vt=(e,t,s,r)=>{const i=e._zod.def;E(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType},En=d("ZodISODateTime",(e,t)=>{Er.init(e,t),I.init(e,t)});function Sn(e){return Ti(En,e)}const An=d("ZodISODate",(e,t)=>{Sr.init(e,t),I.init(e,t)});function Pn(e){return Ni(An,e)}const On=d("ZodISOTime",(e,t)=>{Ar.init(e,t),I.init(e,t)});function Rn(e){return xi(On,e)}const Tn=d("ZodISODuration",(e,t)=>{Pr.init(e,t),I.init(e,t)});function Nn(e){return qi(Tn,e)}const xn=(e,t)=>{_t.init(e,t),e.name="ZodError",Object.defineProperties(e,{format:{value:s=>bs(e,s)},flatten:{value:s=>vs(e,s)},addIssue:{value:s=>{e.issues.push(s),e.message=JSON.stringify(e.issues,ze,2)}},addIssues:{value:s=>{e.issues.push(...s),e.message=JSON.stringify(e.issues,ze,2)}},isEmpty:{get(){return e.issues.length===0}}})},T=d("ZodError",xn,{Parent:Error}),qn=Te(T),$n=Ne(T),Cn=he(T),Mn=fe(T),Un=_s(T),Dn=ks(T),Fn=Is(T),Vn=zs(T),jn=Es(T),Zn=Ss(T),Ln=As(T),Bn=Ps(T),k=d("ZodType",(e,t)=>(_.init(e,t),Object.assign(e["~standard"],{jsonSchema:{input:ue(e,"input"),output:ue(e,"output")}}),e.toJSONSchema=tn(e,{}),e.def=t,e.type=t.type,Object.defineProperty(e,"_def",{value:t}),e.check=(...s)=>e.clone(F(t,{checks:[...t.checks??[],...s.map(r=>typeof r=="function"?{_zod:{check:r,def:{check:"custom"},onattach:[]}}:r)]}),{parent:!0}),e.with=e.check,e.clone=(s,r)=>V(e,s,r),e.brand=()=>e,e.register=((s,r)=>(s.add(e,r),e)),e.parse=(s,r)=>qn(e,s,r,{callee:e.parse}),e.safeParse=(s,r)=>Cn(e,s,r),e.parseAsync=async(s,r)=>$n(e,s,r,{callee:e.parseAsync}),e.safeParseAsync=async(s,r)=>Mn(e,s,r),e.spa=e.safeParseAsync,e.encode=(s,r)=>Un(e,s,r),e.decode=(s,r)=>Dn(e,s,r),e.encodeAsync=async(s,r)=>Fn(e,s,r),e.decodeAsync=async(s,r)=>Vn(e,s,r),e.safeEncode=(s,r)=>jn(e,s,r),e.safeDecode=(s,r)=>Zn(e,s,r),e.safeEncodeAsync=async(s,r)=>Ln(e,s,r),e.safeDecodeAsync=async(s,r)=>Bn(e,s,r),e.refine=(s,r)=>e.check(Va(s,r)),e.superRefine=s=>e.check(ja(s)),e.overwrite=s=>e.check(Y(s)),e.optional=()=>at(e),e.exactOptional=()=>Pa(e),e.nullable=()=>ot(e),e.nullish=()=>at(ot(e)),e.nonoptional=s=>qa(e,s),e.array=()=>C(e),e.or=s=>va([e,s]),e.and=s=>_a(e,s),e.transform=s=>ct(e,Sa(s)),e.default=s=>Ta(e,s),e.prefault=s=>xa(e,s),e.catch=s=>Ca(e,s),e.pipe=s=>ct(e,s),e.readonly=()=>Da(e),e.describe=s=>{const r=e.clone();return Q.add(r,{description:s}),r},Object.defineProperty(e,"description",{get(){return Q.get(e)?.description},configurable:!0}),e.meta=(...s)=>{if(s.length===0)return Q.get(e);const r=e.clone();return Q.add(r,s[0]),r},e.isOptional=()=>e.safeParse(void 0).success,e.isNullable=()=>e.safeParse(null).success,e.apply=s=>s(e),e)),jt=d("_ZodString",(e,t)=>{xe.init(e,t),k.init(e,t),e._zod.processJSONSchema=(r,i,n)=>rn(e,r,i);const s=e._zod.bag;e.format=s.format??null,e.minLength=s.minimum??null,e.maxLength=s.maximum??null,e.regex=(...r)=>e.check(Fi(...r)),e.includes=(...r)=>e.check(Zi(...r)),e.startsWith=(...r)=>e.check(Li(...r)),e.endsWith=(...r)=>e.check(Bi(...r)),e.min=(...r)=>e.check(ce(...r)),e.max=(...r)=>e.check(Ct(...r)),e.length=(...r)=>e.check(Mt(...r)),e.nonempty=(...r)=>e.check(ce(1,...r)),e.lowercase=r=>e.check(Vi(r)),e.uppercase=r=>e.check(ji(r)),e.trim=()=>e.check(Ji()),e.normalize=(...r)=>e.check(Gi(...r)),e.toLowerCase=()=>e.check(Wi()),e.toUpperCase=()=>e.check(Ki()),e.slugify=()=>e.check(Xi())}),Gn=d("ZodString",(e,t)=>{xe.init(e,t),jt.init(e,t),e.email=s=>e.check(di(Jn,s)),e.url=s=>e.check(mi(Wn,s)),e.jwt=s=>e.check(Ri(ua,s)),e.emoji=s=>e.check(wi(Kn,s)),e.guid=s=>e.check(Ye(st,s)),e.uuid=s=>e.check(li(ne,s)),e.uuidv4=s=>e.check(hi(ne,s)),e.uuidv6=s=>e.check(fi(ne,s)),e.uuidv7=s=>e.check(pi(ne,s)),e.nanoid=s=>e.check(vi(Xn,s)),e.guid=s=>e.check(Ye(st,s)),e.cuid=s=>e.check(bi(Hn,s)),e.cuid2=s=>e.check(gi(Yn,s)),e.ulid=s=>e.check(yi(Qn,s)),e.base64=s=>e.check(Ai(aa,s)),e.base64url=s=>e.check(Pi(oa,s)),e.xid=s=>e.check(_i(ea,s)),e.ksuid=s=>e.check(ki(ta,s)),e.ipv4=s=>e.check(Ii(sa,s)),e.ipv6=s=>e.check(zi(ra,s)),e.cidrv4=s=>e.check(Ei(ia,s)),e.cidrv6=s=>e.check(Si(na,s)),e.e164=s=>e.check(Oi(ca,s)),e.datetime=s=>e.check(Sn(s)),e.date=s=>e.check(Pn(s)),e.time=s=>e.check(Rn(s)),e.duration=s=>e.check(Nn(s))});function y(e){return ui(Gn,e)}const I=d("ZodStringFormat",(e,t)=>{g.init(e,t),jt.init(e,t)}),Jn=d("ZodEmail",(e,t)=>{wr.init(e,t),I.init(e,t)}),st=d("ZodGUID",(e,t)=>{pr.init(e,t),I.init(e,t)}),ne=d("ZodUUID",(e,t)=>{mr.init(e,t),I.init(e,t)}),Wn=d("ZodURL",(e,t)=>{vr.init(e,t),I.init(e,t)}),Kn=d("ZodEmoji",(e,t)=>{br.init(e,t),I.init(e,t)}),Xn=d("ZodNanoID",(e,t)=>{gr.init(e,t),I.init(e,t)}),Hn=d("ZodCUID",(e,t)=>{yr.init(e,t),I.init(e,t)}),Yn=d("ZodCUID2",(e,t)=>{_r.init(e,t),I.init(e,t)}),Qn=d("ZodULID",(e,t)=>{kr.init(e,t),I.init(e,t)}),ea=d("ZodXID",(e,t)=>{Ir.init(e,t),I.init(e,t)}),ta=d("ZodKSUID",(e,t)=>{zr.init(e,t),I.init(e,t)}),sa=d("ZodIPv4",(e,t)=>{Or.init(e,t),I.init(e,t)}),ra=d("ZodIPv6",(e,t)=>{Rr.init(e,t),I.init(e,t)}),ia=d("ZodCIDRv4",(e,t)=>{Tr.init(e,t),I.init(e,t)}),na=d("ZodCIDRv6",(e,t)=>{Nr.init(e,t),I.init(e,t)}),aa=d("ZodBase64",(e,t)=>{xr.init(e,t),I.init(e,t)}),oa=d("ZodBase64URL",(e,t)=>{$r.init(e,t),I.init(e,t)}),ca=d("ZodE164",(e,t)=>{Cr.init(e,t),I.init(e,t)}),ua=d("ZodJWT",(e,t)=>{Ur.init(e,t),I.init(e,t)}),Zt=d("ZodNumber",(e,t)=>{Tt.init(e,t),k.init(e,t),e._zod.processJSONSchema=(r,i,n)=>nn(e,r,i),e.gt=(r,i)=>e.check(et(r,i)),e.gte=(r,i)=>e.check(_e(r,i)),e.min=(r,i)=>e.check(_e(r,i)),e.lt=(r,i)=>e.check(Qe(r,i)),e.lte=(r,i)=>e.check(ye(r,i)),e.max=(r,i)=>e.check(ye(r,i)),e.int=r=>e.check(rt(r)),e.safe=r=>e.check(rt(r)),e.positive=r=>e.check(et(0,r)),e.nonnegative=r=>e.check(_e(0,r)),e.negative=r=>e.check(Qe(0,r)),e.nonpositive=r=>e.check(ye(0,r)),e.multipleOf=(r,i)=>e.check(tt(r,i)),e.step=(r,i)=>e.check(tt(r,i)),e.finite=()=>e;const s=e._zod.bag;e.minValue=Math.max(s.minimum??Number.NEGATIVE_INFINITY,s.exclusiveMinimum??Number.NEGATIVE_INFINITY)??null,e.maxValue=Math.min(s.maximum??Number.POSITIVE_INFINITY,s.exclusiveMaximum??Number.POSITIVE_INFINITY)??null,e.isInt=(s.format??"").includes("int")||Number.isSafeInteger(s.multipleOf??.5),e.isFinite=!0,e.format=s.format??null});function $(e){return $i(Zt,e)}const da=d("ZodNumberFormat",(e,t)=>{Dr.init(e,t),Zt.init(e,t)});function rt(e){return Ci(da,e)}const la=d("ZodBoolean",(e,t)=>{Fr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>an(e,s,r)});function j(e){return Mi(la,e)}const ha=d("ZodUnknown",(e,t)=>{Vr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>cn()});function it(){return Ui(ha)}const fa=d("ZodNever",(e,t)=>{jr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>on(e,s,r)});function pa(e){return Di(fa,e)}const ma=d("ZodArray",(e,t)=>{Zr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>fn(e,s,r,i),e.element=t.element,e.min=(s,r)=>e.check(ce(s,r)),e.nonempty=s=>e.check(ce(1,s)),e.max=(s,r)=>e.check(Ct(s,r)),e.length=(s,r)=>e.check(Mt(s,r)),e.unwrap=()=>e.element});function C(e,t){return Hi(ma,e,t)}const wa=d("ZodObject",(e,t)=>{Br.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>pn(e,s,r,i),b(e,"shape",()=>t.shape),e.keyof=()=>Ia(Object.keys(e._zod.def.shape)),e.catchall=s=>e.clone({...e._zod.def,catchall:s}),e.passthrough=()=>e.clone({...e._zod.def,catchall:it()}),e.loose=()=>e.clone({...e._zod.def,catchall:it()}),e.strict=()=>e.clone({...e._zod.def,catchall:pa()}),e.strip=()=>e.clone({...e._zod.def,catchall:void 0}),e.extend=s=>hs(e,s),e.safeExtend=s=>fs(e,s),e.merge=s=>ps(e,s),e.pick=s=>ds(e,s),e.omit=s=>ls(e,s),e.partial=(...s)=>ms(Bt,e,s[0]),e.required=(...s)=>ws(Gt,e,s[0])});function z(e,t){const s={type:"object",shape:e??{},...p(t)};return new wa(s)}const Lt=d("ZodUnion",(e,t)=>{qt.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>mn(e,s,r,i),e.options=t.options});function va(e,t){return new Lt({type:"union",options:e,...p(t)})}const ba=d("ZodDiscriminatedUnion",(e,t)=>{Lt.init(e,t),Gr.init(e,t)});function ga(e,t,s){return new ba({type:"union",options:t,discriminator:e,...p(s)})}const ya=d("ZodIntersection",(e,t)=>{Jr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>wn(e,s,r,i)});function _a(e,t){return new ya({type:"intersection",left:e,right:t})}const ka=d("ZodRecord",(e,t)=>{Wr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>vn(e,s,r,i),e.keyType=t.keyType,e.valueType=t.valueType});function qe(e,t,s){return new ka({type:"record",keyType:e,valueType:t,...p(s)})}const Se=d("ZodEnum",(e,t)=>{Kr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(r,i,n)=>un(e,r,i),e.enum=t.entries,e.options=Object.values(t.entries);const s=new Set(Object.keys(t.entries));e.extract=(r,i)=>{const n={};for(const a of r)if(s.has(a))n[a]=t.entries[a];else throw new Error(`Key ${a} not found in enum`);return new Se({...t,checks:[],...p(i),entries:n})},e.exclude=(r,i)=>{const n={...t.entries};for(const a of r)if(s.has(a))delete n[a];else throw new Error(`Key ${a} not found in enum`);return new Se({...t,checks:[],...p(i),entries:n})}});function Ia(e,t){const s=Array.isArray(e)?Object.fromEntries(e.map(r=>[r,r])):e;return new Se({type:"enum",entries:s,...p(t)})}const za=d("ZodLiteral",(e,t)=>{Xr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>dn(e,s,r),e.values=new Set(t.values),Object.defineProperty(e,"value",{get(){if(t.values.length>1)throw new Error("This schema contains multiple valid literal values. Use `.values` instead.");return t.values[0]}})});function nt(e,t){return new za({type:"literal",values:Array.isArray(e)?e:[e],...p(t)})}const Ea=d("ZodTransform",(e,t)=>{Hr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>hn(e,s),e._zod.parse=(s,r)=>{if(r.direction==="backward")throw new mt(e.constructor.name);s.addIssue=n=>{if(typeof n=="string")s.issues.push(te(n,s.value,t));else{const a=n;a.fatal&&(a.continue=!1),a.code??(a.code="custom"),a.input??(a.input=s.value),a.inst??(a.inst=e),s.issues.push(te(a))}};const i=t.transform(s.value,s);return i instanceof Promise?i.then(n=>(s.value=n,s)):(s.value=i,s)}});function Sa(e){return new Ea({type:"transform",transform:e})}const Bt=d("ZodOptional",(e,t)=>{$t.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Vt(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function at(e){return new Bt({type:"optional",innerType:e})}const Aa=d("ZodExactOptional",(e,t)=>{Yr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Vt(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function Pa(e){return new Aa({type:"optional",innerType:e})}const Oa=d("ZodNullable",(e,t)=>{Qr.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>bn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function ot(e){return new Oa({type:"nullable",innerType:e})}const Ra=d("ZodDefault",(e,t)=>{ei.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>yn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType,e.removeDefault=e.unwrap});function Ta(e,t){return new Ra({type:"default",innerType:e,get defaultValue(){return typeof t=="function"?t():gt(t)}})}const Na=d("ZodPrefault",(e,t)=>{ti.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>_n(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function xa(e,t){return new Na({type:"prefault",innerType:e,get defaultValue(){return typeof t=="function"?t():gt(t)}})}const Gt=d("ZodNonOptional",(e,t)=>{si.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>gn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function qa(e,t){return new Gt({type:"nonoptional",innerType:e,...p(t)})}const $a=d("ZodCatch",(e,t)=>{ri.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>kn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType,e.removeCatch=e.unwrap});function Ca(e,t){return new $a({type:"catch",innerType:e,catchValue:typeof t=="function"?t:()=>t})}const Ma=d("ZodPipe",(e,t)=>{ii.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>In(e,s,r,i),e.in=t.in,e.out=t.out});function ct(e,t){return new Ma({type:"pipe",in:e,out:t})}const Ua=d("ZodReadonly",(e,t)=>{ni.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>zn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function Da(e){return new Ua({type:"readonly",innerType:e})}const Fa=d("ZodCustom",(e,t)=>{ai.init(e,t),k.init(e,t),e._zod.processJSONSchema=(s,r,i)=>ln(e,s)});function Va(e,t={}){return Yi(Fa,e,t)}function ja(e){return Qi(e)}$().int().nonnegative().max(255).brand("u8");const P=$().int().nonnegative().max(Number.MAX_SAFE_INTEGER).brand("u53");function M(e){return P.parse(e)}const Jt=ga("kind",[z({kind:nt("legacy")}),z({kind:nt("cmaf"),timescale:P,trackId:P})]).default({kind:"legacy"}),Za=z({name:y()}),ut=z({codec:y(),container:Jt,description:y().optional(),sampleRate:P,numberOfChannels:P,bitrate:P.optional(),jitter:P.optional()}),La=z({renditions:qe(y(),ut)}).or(z({track:Za,config:ut}).transform(e=>({renditions:{[e.track.name]:e.config}}))),Ba=z({hardware:C(y()).optional(),software:C(y()).optional(),unsupported:C(y()).optional()}),Ga=z({hardware:C(y()).optional(),software:C(y()).optional(),unsupported:C(y()).optional()}),Ja=z({video:Ba.optional(),audio:Ga.optional()}),se=z({name:y()}),Wa=z({message:se.optional(),typing:se.optional()}),$e=z({x:$().optional(),y:$().optional(),z:$().optional(),s:$().optional()}),Ka=z({initial:$e.optional(),track:se.optional(),handle:y().optional(),peers:se.optional()}),Xa=qe(y(),$e);z({name:y().optional(),avatar:y().optional(),audio:j().optional(),video:j().optional(),typing:j().optional(),chat:j().optional(),screen:j().optional()});const L={chat:90,audio:80,video:60,typing:40,location:20,preview:10},Ha=z({id:y().optional(),name:y().optional(),avatar:y().optional(),color:y().optional()}),Ya=z({name:y()}),dt=z({codec:y(),container:Jt,description:y().optional(),codedWidth:P.optional(),codedHeight:P.optional(),displayAspectWidth:P.optional(),displayAspectHeight:P.optional(),framerate:$().optional(),bitrate:P.optional(),optimizeForLatency:j().optional(),jitter:P.optional()}),Qa=z({renditions:qe(y(),dt),display:z({width:P,height:P}).optional(),rotation:$().optional(),flip:j().optional()}).or(C(z({track:Ya,config:dt})).transform(e=>{const t=e[0]?.config;return{renditions:Object.fromEntries(e.map(s=>[s.track.name,s.config])),display:t?.displayAspectWidth&&t?.displayAspectHeight?{width:t.displayAspectWidth,height:t.displayAspectHeight}:void 0,rotation:void 0,flip:void 0}}));z({video:Qa.optional(),audio:La.optional(),location:Ka.optional(),user:Ha.optional(),chat:Wa.optional(),capabilities:Ja.optional(),preview:se.optional()});function lt(e){return new TextEncoder().encode(JSON.stringify(e))}class me{#e;#t;constructor(t){this.#e=t}encode(t,s,r){if(r)this.#t?.close(),this.#t=this.#e.appendGroup();else if(!this.#t)throw new Error("must start with a keyframe");this.#t?.writeFrame(me.#s(t,s))}static#s(t,s){const r=ss(s),i=t.byteLength,n=new Uint8Array(r.byteLength+i);return n.set(r,0),t instanceof Uint8Array?n.set(t,r.byteLength):t.copyTo(n.subarray(r.byteLength)),n}close(t){this.#e.close(t),this.#t?.close()}}navigator.userAgent.toLowerCase().includes("chrome");const eo=navigator.userAgent.toLowerCase().includes("firefox");let ke;async function to(){return globalThis.AudioEncoder&&globalThis.AudioDecoder?!0:(ke||(console.warn("using Opus polyfill; performance may be degraded"),ke=Promise.all([Ue(()=>import("./libav-opus-af-BlMWboA7-B4GfDr9_.js"),[]),Ue(()=>import("./main-DGBFe0O7-DQ8if_La.js"),[])]).then(async([e,t])=>(await t.load({LibAV:e,polyfill:!0}),!0))),await ke)}const so=`var __defProp = Object.defineProperty;
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};

// ../../node_modules/.bun/dequal@2.0.3/node_modules/dequal/dist/index.mjs
var has = Object.prototype.hasOwnProperty;
function find(iter, tar, key) {
  for (key of iter.keys()) {
    if (dequal(key, tar)) return key;
  }
}
function dequal(foo, bar) {
  var ctor, len, tmp;
  if (foo === bar) return true;
  if (foo && bar && (ctor = foo.constructor) === bar.constructor) {
    if (ctor === Date) return foo.getTime() === bar.getTime();
    if (ctor === RegExp) return foo.toString() === bar.toString();
    if (ctor === Array) {
      if ((len = foo.length) === bar.length) {
        while (len-- && dequal(foo[len], bar[len])) ;
      }
      return len === -1;
    }
    if (ctor === Set) {
      if (foo.size !== bar.size) {
        return false;
      }
      for (len of foo) {
        tmp = len;
        if (tmp && typeof tmp === "object") {
          tmp = find(bar, tmp);
          if (!tmp) return false;
        }
        if (!bar.has(tmp)) return false;
      }
      return true;
    }
    if (ctor === Map) {
      if (foo.size !== bar.size) {
        return false;
      }
      for (len of foo) {
        tmp = len[0];
        if (tmp && typeof tmp === "object") {
          tmp = find(bar, tmp);
          if (!tmp) return false;
        }
        if (!dequal(len[1], bar.get(tmp))) {
          return false;
        }
      }
      return true;
    }
    if (ctor === ArrayBuffer) {
      foo = new Uint8Array(foo);
      bar = new Uint8Array(bar);
    } else if (ctor === DataView) {
      if ((len = foo.byteLength) === bar.byteLength) {
        while (len-- && foo.getInt8(len) === bar.getInt8(len)) ;
      }
      return len === -1;
    }
    if (ArrayBuffer.isView(foo)) {
      if ((len = foo.byteLength) === bar.byteLength) {
        while (len-- && foo[len] === bar[len]) ;
      }
      return len === -1;
    }
    if (!ctor || typeof foo === "object") {
      len = 0;
      for (ctor in foo) {
        if (has.call(foo, ctor) && ++len && !has.call(bar, ctor)) return false;
        if (!(ctor in bar) || !dequal(foo[ctor], bar[ctor])) return false;
      }
      return Object.keys(bar).length === len;
    }
  }
  return foo !== foo && bar !== bar;
}

// ../signals/src/index.ts
var DEV = typeof import.meta.env !== "undefined" && import.meta.env?.MODE !== "production";
var SIGNAL_BRAND = /* @__PURE__ */ Symbol.for("@moq/signals");
var Signal = class _Signal {
  #value;
  #subscribers = /* @__PURE__ */ new Set();
  #changed = /* @__PURE__ */ new Set();
  // Brand to identify this as a Signal across package instances
  [SIGNAL_BRAND] = true;
  constructor(value) {
    this.#value = value;
  }
  static from(value) {
    if (typeof value === "object" && value !== null && SIGNAL_BRAND in value) {
      return value;
    }
    return new _Signal(value);
  }
  get() {
    return this.#value;
  }
  // TODO rename to \`get\` once we've ported everything
  peek() {
    return this.#value;
  }
  // Set the current value, by default notifying subscribers if the value is different.
  // If notify is undefined, we'll check if the value has changed after the microtask.
  set(value, notify) {
    const old = this.#value;
    this.#value = value;
    if (notify === false) return;
    if (notify === void 0 && old === this.#value) {
      if (DEV && value !== null && (typeof value === "object" || typeof value === "function")) {
        console.warn(
          "Signal.set() called with the same object reference. Changes won't propagate. Use update() or mutate() instead."
        );
      }
      return;
    }
    if (this.#subscribers.size === 0 && this.#changed.size === 0) return;
    const subscribers = this.#subscribers;
    const changed = this.#changed;
    this.#changed = /* @__PURE__ */ new Set();
    queueMicrotask(() => {
      if (notify === void 0 && dequal(old, this.#value)) {
        for (const fn of changed) {
          this.#changed.add(fn);
        }
        return;
      }
      for (const fn of subscribers) {
        try {
          fn(value);
        } catch (error2) {
          console.error("signal subscriber error", error2);
        }
      }
      for (const fn of changed) {
        try {
          fn(value);
        } catch (error2) {
          console.error("signal changed error", error2);
        }
      }
    });
  }
  // Mutate the current value and notify subscribers unless notify is false.
  // Unlike set, we can't use a dequal check because the function may mutate the value.
  update(fn, notify = true) {
    const value = fn(this.#value);
    this.set(value, notify);
  }
  // Mutate the current value and notify subscribers unless notify is false.
  mutate(fn, notify = true) {
    const r = fn(this.#value);
    this.set(this.#value, notify);
    return r;
  }
  // Receive a notification each time the value changes.
  subscribe(fn) {
    this.#subscribers.add(fn);
    if (DEV && this.#subscribers.size >= 100 && Number.isInteger(Math.log10(this.#subscribers.size))) {
      throw new Error("signal has too many subscribers; may be leaking");
    }
    return () => this.#subscribers.delete(fn);
  }
  // Receive a notification when the value changes.
  changed(fn) {
    this.#changed.add(fn);
    return () => this.#changed.delete(fn);
  }
  // Receive a notification when the value changes AND with the initial value.
  watch(fn) {
    const dispose = this.subscribe(fn);
    queueMicrotask(() => fn(this.#value));
    return dispose;
  }
  static async race(...sigs) {
    const dispose = [];
    const result = await new Promise((resolve) => {
      for (const sig of sigs) {
        dispose.push(sig.changed(resolve));
      }
    });
    for (const fn of dispose) fn();
    return result;
  }
};
var Effect = class _Effect {
  // Sanity check to make sure roots are being disposed on dev.
  static #finalizer = new FinalizationRegistry((debugInfo) => {
    console.warn(\`Signals was garbage collected without being closed:
\${debugInfo}\`);
  });
  #fn;
  #dispose = [];
  #unwatch = [];
  #async = [];
  #stack;
  #scheduled = false;
  #stop;
  #stopped;
  #close;
  #closed;
  // If a function is provided, it will be run with the effect as an argument.
  constructor(fn) {
    if (DEV) {
      const debug = new Error("created here:").stack ?? "No stack";
      _Effect.#finalizer.register(this, debug, this);
    }
    this.#fn = fn;
    if (DEV) {
      this.#stack = new Error().stack;
    }
    this.#stopped = new Promise((resolve) => {
      this.#stop = resolve;
    });
    this.#closed = new Promise((resolve) => {
      this.#close = resolve;
    });
    if (fn) {
      this.#schedule();
    }
  }
  #schedule() {
    if (this.#scheduled) return;
    this.#scheduled = true;
    queueMicrotask(
      () => this.#run().catch((error2) => {
        console.error("effect error", error2, this.#stack);
      })
    );
  }
  async #run() {
    if (this.#dispose === void 0) return;
    this.#stop();
    this.#stopped = new Promise((resolve) => {
      this.#stop = resolve;
    });
    for (const unwatch of this.#unwatch) unwatch();
    this.#unwatch.length = 0;
    for (const fn of this.#dispose) fn();
    this.#dispose.length = 0;
    if (this.#async.length > 0) {
      try {
        let warn;
        const timeout = new Promise((resolve) => {
          warn = setTimeout(() => {
            if (DEV) {
              console.warn("spawn is still running after 5s; continuing anyway", this.#stack);
            }
            resolve();
          }, 5e3);
        });
        await Promise.race([Promise.all(this.#async), timeout]);
        if (warn) clearTimeout(warn);
        this.#async.length = 0;
      } catch (error2) {
        console.error("async effect error", error2);
        if (this.#stack) console.error("stack", this.#stack);
      }
    }
    if (this.#dispose === void 0) return;
    this.#scheduled = false;
    if (this.#fn) {
      this.#fn(this);
      if (DEV && this.#dispose !== void 0 && this.#unwatch.length === 0 && this.#dispose.length === 0 && this.#async.length === 0) {
        console.warn("Effect did not subscribe to any signals; it will never rerun.", this.#stack);
      }
    }
  }
  // Get the current value of a signal, monitoring it for changes (via ===) and rerunning on change.
  get(signal) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.get called when closed, returning current value");
      }
      return signal.peek();
    }
    const value = signal.peek();
    const dispose = signal.changed(() => this.#schedule());
    this.#unwatch.push(dispose);
    return value;
  }
  // Temporarily set the value of a signal, unsetting it on cleanup.
  // The last argument is the cleanup value, set before the effect is rerun.
  // It's optional only if T can be undefined.
  set(signal, value, ...args) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.set called when closed, ignoring");
      }
      return;
    }
    signal.set(value);
    const cleanup = args[0];
    const cleanupValue = cleanup === void 0 ? void 0 : cleanup;
    this.cleanup(() => signal.set(cleanupValue));
  }
  // Spawn an async effect that blocks the effect being reloaded until it completes.
  // Use this.cancel if you need to detect when the effect is reloading to terminate.
  // TODO: Add effect for another layer of nesting
  spawn(fn) {
    const promise = fn().catch((error2) => {
      console.error("spawn error", error2);
    });
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.spawn called when closed");
      }
      return;
    }
    this.#async.push(promise);
  }
  // Run the function after the given delay in milliseconds UNLESS the effect is cleaned up first.
  timer(fn, ms) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.timer called when closed, ignoring");
      }
      return;
    }
    let timeout;
    timeout = setTimeout(() => {
      timeout = void 0;
      fn();
    }, ms);
    this.cleanup(() => timeout && clearTimeout(timeout));
  }
  // Run the function, and clean up the nested effect after the given delay.
  timeout(fn, ms) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.timeout called when closed, ignoring");
      }
      return;
    }
    const effect = new _Effect(fn);
    let timeout = setTimeout(() => {
      effect.close();
      timeout = void 0;
    }, ms);
    this.#dispose.push(() => {
      if (timeout) {
        clearTimeout(timeout);
        effect.close();
      }
    });
  }
  // Run the callback on the next animation frame, unless the effect is cleaned up first.
  animate(fn) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.animate called when closed, ignoring");
      }
      return;
    }
    let animate = requestAnimationFrame((now) => {
      fn(now);
      animate = void 0;
    });
    this.cleanup(() => {
      if (animate) cancelAnimationFrame(animate);
    });
  }
  interval(fn, ms) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.interval called when closed, ignoring");
      }
      return;
    }
    const interval = setInterval(() => {
      fn();
    }, ms);
    this.cleanup(() => clearInterval(interval));
  }
  // Create a nested effect that can be rerun independently.
  run(fn) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.nested called when closed, ignoring");
      }
      return;
    }
    const effect = new _Effect(fn);
    this.#dispose.push(() => effect.close());
  }
  // Backwards compatibility with the old name.
  effect(fn) {
    return this.run(fn);
  }
  // Get the values of multiple signals, returning undefined if any are falsy.
  getAll(signals) {
    const values = [];
    for (const signal of signals) {
      const value = this.get(signal);
      if (!value) return void 0;
      values.push(value);
    }
    return values;
  }
  // A helper to call a function when a signal changes.
  subscribe(signal, fn) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.subscribe called when closed, running once");
      }
      fn(signal.peek());
      return;
    }
    this.run((effect) => {
      const value = effect.get(signal);
      fn(value);
    });
  }
  event(target, type, listener, options) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.eventListener called when closed, ignoring");
      }
      return;
    }
    target.addEventListener(type, listener, options);
    this.cleanup(() => target.removeEventListener(type, listener, options));
  }
  // Register a cleanup function.
  cleanup(fn) {
    if (this.#dispose === void 0) {
      if (DEV) {
        console.warn("Effect.cleanup called when closed, running immediately");
      }
      fn();
      return;
    }
    this.#dispose.push(fn);
  }
  close() {
    if (this.#dispose === void 0) {
      return;
    }
    this.#close();
    this.#stop();
    for (const fn of this.#dispose) fn();
    this.#dispose = void 0;
    for (const signal of this.#unwatch) signal();
    this.#unwatch.length = 0;
    this.#async.length = 0;
    if (DEV) {
      _Effect.#finalizer.unregister(this);
    }
  }
  get closed() {
    return this.#closed;
  }
  get cancel() {
    return this.#stopped;
  }
  proxy(dst, src) {
    this.subscribe(src, (value) => dst.update(() => value));
  }
};

// ../lite/src/path.ts
function from(...paths) {
  const joined = paths.join("/");
  return joined.replace(/\\/+/g, "/").replace(/^\\/+/, "").replace(/\\/+$/, "");
}

// ../../node_modules/.bun/@moq+web-transport-ws@0.1.2/node_modules/@moq/web-transport-ws/varint.js
var VarInt = class _VarInt {
  static MAX = (1n << 62n) - 1n;
  static MAX_SIZE = 8;
  value;
  constructor(value) {
    if (value < 0n || value > _VarInt.MAX) {
      throw new Error(\`VarInt value out of range: \${value}\`);
    }
    this.value = value;
  }
  static from(value) {
    return new _VarInt(BigInt(value));
  }
  size() {
    const x = this.value;
    if (x < 2n ** 6n)
      return 1;
    if (x < 2n ** 14n)
      return 2;
    if (x < 2n ** 30n)
      return 4;
    if (x < 2n ** 62n)
      return 8;
    throw new Error("VarInt value too large");
  }
  // Append to the provided buffer
  encode(dst) {
    const x = this.value;
    const size = this.size();
    if (dst.byteOffset + dst.byteLength + size > dst.buffer.byteLength) {
      throw new Error("destination buffer too small");
    }
    const view = new DataView(dst.buffer, dst.byteOffset + dst.byteLength, size);
    if (size === 1) {
      view.setUint8(0, Number(x));
    } else if (size === 2) {
      view.setUint16(0, 1 << 14 | Number(x), false);
    } else if (size === 4) {
      view.setUint32(0, 2 << 30 | Number(x), false);
    } else if (size === 8) {
      view.setBigUint64(0, 3n << 62n | x, false);
    } else {
      throw new Error("VarInt value too large");
    }
    return new Uint8Array(dst.buffer, dst.byteOffset, dst.byteLength + size);
  }
  static decode(buffer) {
    if (buffer.byteLength < 1) {
      throw new Error("Unexpected end of buffer");
    }
    const view = new DataView(buffer.buffer, buffer.byteOffset);
    const firstByte = view.getUint8(0);
    const tag = firstByte >> 6;
    let value;
    let bytesRead;
    switch (tag) {
      case 0:
        value = BigInt(firstByte & 63);
        bytesRead = 1;
        break;
      case 1:
        if (2 > buffer.length) {
          throw new Error("Unexpected end of buffer");
        }
        value = BigInt(view.getUint16(0, false) & 16383);
        bytesRead = 2;
        break;
      case 2:
        if (4 > buffer.length) {
          throw new Error("Unexpected end of buffer");
        }
        value = BigInt(view.getUint32(0, false) & 1073741823);
        bytesRead = 4;
        break;
      case 3:
        if (8 > buffer.length) {
          throw new Error("Unexpected end of buffer");
        }
        value = view.getBigUint64(0, false) & 0x3fffffffffffffffn;
        bytesRead = 8;
        break;
      default:
        throw new Error("Invalid VarInt tag");
    }
    const remaining = new Uint8Array(buffer.buffer, buffer.byteOffset + bytesRead, buffer.byteLength - bytesRead);
    return [new _VarInt(value), remaining];
  }
};

// ../lite/src/varint.ts
var MAX_U6 = 2 ** 6 - 1;
var MAX_U14 = 2 ** 14 - 1;
var MAX_U30 = 2 ** 30 - 1;
var MAX_U53 = Number.MAX_SAFE_INTEGER;
function setUint8(dst, v) {
  const buffer = new Uint8Array(dst, 0, 1);
  buffer[0] = v;
  return buffer;
}
function setUint16(dst, v) {
  const view = new DataView(dst, 0, 2);
  view.setUint16(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}
function setUint32(dst, v) {
  const view = new DataView(dst, 0, 4);
  view.setUint32(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}
function setUint64(dst, v) {
  const view = new DataView(dst, 0, 8);
  view.setBigUint64(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}
var MAX_U62 = 2n ** 62n - 1n;
function encodeTo(dst, v) {
  const b = BigInt(v);
  if (b < 0n) {
    throw new Error(\`underflow, value is negative: \${v}\`);
  }
  if (b > MAX_U62) {
    throw new Error(\`overflow, value larger than 62-bits: \${v}\`);
  }
  const n = Number(b);
  if (n <= MAX_U6) {
    return setUint8(dst, n);
  }
  if (n <= MAX_U14) {
    return setUint16(dst, n | 16384);
  }
  if (n <= MAX_U30) {
    return setUint32(dst, n | 2147483648);
  }
  return setUint64(dst, b | 0xc000000000000000n);
}
function encode2(v) {
  return encodeTo(new ArrayBuffer(8), v);
}
function decode2(buf) {
  if (buf.length === 0) {
    throw new Error("buffer is empty");
  }
  const size = 1 << ((buf[0] & 192) >> 6);
  if (buf.length < size) {
    throw new Error(\`buffer too short: need \${size} bytes, have \${buf.length}\`);
  }
  const view = new DataView(buf.buffer, buf.byteOffset, size);
  const remain = buf.subarray(size);
  let v;
  if (size === 1) {
    v = buf[0] & 63;
  } else if (size === 2) {
    v = view.getUint16(0) & 16383;
  } else if (size === 4) {
    v = view.getUint32(0) & 1073741823;
  } else if (size === 8) {
    v = Number(view.getBigUint64(0) & 0x3fffffffffffffffn);
  } else {
    throw new Error("impossible");
  }
  return [v, remain];
}

// ../lite/src/stream.ts
var MAX_U31 = 2 ** 31 - 1;
var MAX_READ_SIZE = 1024 * 1024 * 64;
var Reader = class {
  #buffer;
  #stream;
  // if undefined, the buffer is consumed then EOF
  #reader;
  constructor(stream, buffer) {
    this.#buffer = buffer ?? new Uint8Array();
    this.#stream = stream;
    this.#reader = this.#stream?.getReader();
  }
  // Adds more data to the buffer, returning true if more data was added.
  async #fill() {
    if (!this.#reader) {
      return false;
    }
    const result = await this.#reader.read();
    if (result.done) {
      return false;
    }
    if (result.value.byteLength === 0) {
      throw new Error("unexpected empty chunk");
    }
    const buffer = new Uint8Array(result.value);
    if (this.#buffer.byteLength === 0) {
      this.#buffer = buffer;
    } else {
      const temp = new Uint8Array(this.#buffer.byteLength + buffer.byteLength);
      temp.set(this.#buffer);
      temp.set(buffer, this.#buffer.byteLength);
      this.#buffer = temp;
    }
    return true;
  }
  // Add more data to the buffer until it's at least size bytes.
  async #fillTo(size) {
    if (size > MAX_READ_SIZE) {
      throw new Error(\`read size \${size} exceeds max size \${MAX_READ_SIZE}\`);
    }
    while (this.#buffer.byteLength < size) {
      if (!await this.#fill()) {
        throw new Error("unexpected end of stream");
      }
    }
  }
  // Consumes the first size bytes of the buffer.
  #slice(size) {
    const result = new Uint8Array(this.#buffer.buffer, this.#buffer.byteOffset, size);
    this.#buffer = new Uint8Array(
      this.#buffer.buffer,
      this.#buffer.byteOffset + size,
      this.#buffer.byteLength - size
    );
    return result;
  }
  async read(size) {
    if (size === 0) return new Uint8Array();
    await this.#fillTo(size);
    return this.#slice(size);
  }
  async readAll() {
    while (await this.#fill()) {
    }
    return this.#slice(this.#buffer.byteLength);
  }
  async string() {
    const length = await this.u53();
    const buffer = await this.read(length);
    return new TextDecoder().decode(buffer);
  }
  async bool() {
    const v = await this.u8();
    if (v === 0) return false;
    if (v === 1) return true;
    throw new Error("invalid bool value");
  }
  async u8() {
    await this.#fillTo(1);
    return this.#slice(1)[0];
  }
  async u16() {
    await this.#fillTo(2);
    const view = new DataView(this.#buffer.buffer, this.#buffer.byteOffset, 2);
    const result = view.getUint16(0);
    this.#slice(2);
    return result;
  }
  // Returns a Number using 53-bits, the max Javascript can use for integer math
  async u53() {
    const v = await this.u62();
    if (v > MAX_U53) {
      throw new Error("value larger than 53-bits; use v62 instead");
    }
    return Number(v);
  }
  // NOTE: Returns a bigint instead of a number since it may be larger than 53-bits
  async u62() {
    await this.#fillTo(1);
    const size = (this.#buffer[0] & 192) >> 6;
    if (size === 0) {
      const first = this.#slice(1)[0];
      return BigInt(first) & 0x3fn;
    }
    if (size === 1) {
      await this.#fillTo(2);
      const slice2 = this.#slice(2);
      const view2 = new DataView(slice2.buffer, slice2.byteOffset, slice2.byteLength);
      return BigInt(view2.getUint16(0)) & 0x3fffn;
    }
    if (size === 2) {
      await this.#fillTo(4);
      const slice2 = this.#slice(4);
      const view2 = new DataView(slice2.buffer, slice2.byteOffset, slice2.byteLength);
      return BigInt(view2.getUint32(0)) & 0x3fffffffn;
    }
    await this.#fillTo(8);
    const slice = this.#slice(8);
    const view = new DataView(slice.buffer, slice.byteOffset, slice.byteLength);
    return view.getBigUint64(0) & 0x3fffffffffffffffn;
  }
  // Returns false if there is more data to read, blocking if it hasn't been received yet.
  async done() {
    if (this.#buffer.byteLength > 0) return false;
    return !await this.#fill();
  }
  stop(reason) {
    this.#reader?.cancel(reason).catch(() => void 0);
  }
  get closed() {
    return this.#reader?.closed ?? Promise.resolve();
  }
};
var Writer = class _Writer {
  #writer;
  #stream;
  // Scratch buffer for writing varints.
  // Fixed at 8 bytes.
  #scratch;
  constructor(stream) {
    this.#stream = stream;
    this.#scratch = new ArrayBuffer(8);
    this.#writer = this.#stream.getWriter();
  }
  async bool(v) {
    await this.write(setUint82(this.#scratch, v ? 1 : 0));
  }
  async u8(v) {
    await this.write(setUint82(this.#scratch, v));
  }
  async u16(v) {
    await this.write(setUint162(this.#scratch, v));
  }
  async i32(v) {
    if (Math.abs(v) > MAX_U31) {
      throw new Error(\`overflow, value larger than 32-bits: \${v.toString()}\`);
    }
    await this.write(setInt32(this.#scratch, v));
  }
  async u53(v) {
    if (v > MAX_U53) {
      throw new Error(\`overflow, value larger than 53-bits: \${v.toString()}\`);
    }
    await this.write(encodeTo(this.#scratch, v));
  }
  async u62(v) {
    await this.write(encodeTo(this.#scratch, v));
  }
  async write(v) {
    await this.#writer.write(v);
  }
  async string(str) {
    const data = new TextEncoder().encode(str);
    await this.u53(data.byteLength);
    await this.write(data);
  }
  close() {
    this.#writer.close().catch(() => void 0);
  }
  get closed() {
    return this.#writer.closed;
  }
  reset(reason) {
    this.#writer.abort(reason).catch(() => void 0);
  }
  static async open(quic) {
    const writable = await quic.createUnidirectionalStream();
    return new _Writer(writable);
  }
};
function setUint82(dst, v) {
  const buffer = new Uint8Array(dst, 0, 1);
  buffer[0] = v;
  return buffer;
}
function setUint162(dst, v) {
  const view = new DataView(dst, 0, 2);
  view.setUint16(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}
function setInt32(dst, v) {
  const view = new DataView(dst, 0, 4);
  view.setInt32(0, v);
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}

// ../lite/src/util/error.ts
function unreachable(value) {
  throw new Error(\`unreachable: \${value}\`);
}

// ../../node_modules/.bun/async-mutex@0.5.0/node_modules/async-mutex/index.mjs
var E_TIMEOUT = new Error("timeout while waiting for mutex to become available");
var E_ALREADY_LOCKED = new Error("mutex already locked");
var E_CANCELED = new Error("request for lock canceled");

// ../lite/src/ietf/message.ts
async function encode3(writer, f) {
  let scratch = new Uint8Array();
  const temp = new Writer(
    new WritableStream({
      write(chunk) {
        const needed = scratch.byteLength + chunk.byteLength;
        if (needed > scratch.buffer.byteLength) {
          const capacity = Math.max(needed, scratch.buffer.byteLength * 2);
          const newBuffer = new ArrayBuffer(capacity);
          const newScratch = new Uint8Array(newBuffer, 0, needed);
          newScratch.set(scratch);
          newScratch.set(chunk, scratch.byteLength);
          scratch = newScratch;
        } else {
          scratch = new Uint8Array(scratch.buffer, 0, needed);
          scratch.set(chunk, needed - chunk.byteLength);
        }
      }
    })
  );
  try {
    await f(temp);
  } finally {
    temp.close();
  }
  await temp.closed;
  if (scratch.byteLength > 65535) {
    throw new Error(\`Message too large: \${scratch.byteLength} bytes (max 65535)\`);
  }
  await writer.u16(scratch.byteLength);
  await writer.write(scratch);
}
async function decode3(reader, f) {
  const size = await reader.u16();
  const data = await reader.read(size);
  const limit = new Reader(void 0, data);
  const msg = await f(limit);
  if (!await limit.done()) {
    throw new Error("Message decoding consumed too few bytes");
  }
  return msg;
}

// ../lite/src/ietf/fetch.ts
var Fetch = class _Fetch {
  static id = 22;
  requestId;
  trackNamespace;
  trackName;
  subscriberPriority;
  groupOrder;
  startGroup;
  startObject;
  endGroup;
  endObject;
  constructor({
    requestId,
    trackNamespace,
    trackName,
    subscriberPriority,
    groupOrder,
    startGroup,
    startObject,
    endGroup,
    endObject
  }) {
    this.requestId = requestId;
    this.trackNamespace = trackNamespace;
    this.trackName = trackName;
    this.subscriberPriority = subscriberPriority;
    this.groupOrder = groupOrder;
    this.startGroup = startGroup;
    this.startObject = startObject;
    this.endGroup = endGroup;
    this.endObject = endObject;
  }
  async #encode(_w) {
    throw new Error("FETCH messages are not supported");
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _Fetch.#decode);
  }
  static async #decode(_r) {
    throw new Error("FETCH messages are not supported");
  }
};
var FetchOk = class _FetchOk {
  static id = 24;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(_w) {
    throw new Error("FETCH_OK messages are not supported");
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _FetchOk.#decode);
  }
  static async #decode(_r) {
    throw new Error("FETCH_OK messages are not supported");
  }
};
var FetchError = class _FetchError {
  static id = 25;
  requestId;
  errorCode;
  reasonPhrase;
  constructor({
    requestId,
    errorCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(_w) {
    throw new Error("FETCH_ERROR messages are not supported");
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _FetchError.#decode);
  }
  static async #decode(_r) {
    throw new Error("FETCH_ERROR messages are not supported");
  }
};
var FetchCancel = class _FetchCancel {
  static id = 23;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(_w) {
    throw new Error("FETCH_CANCEL messages are not supported");
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _FetchCancel.#decode);
  }
  static async #decode(_r) {
    throw new Error("FETCH_CANCEL messages are not supported");
  }
};

// ../lite/src/ietf/goaway.ts
var GoAway = class _GoAway {
  static id = 16;
  newSessionUri;
  constructor({ newSessionUri }) {
    this.newSessionUri = newSessionUri;
  }
  async #encode(w) {
    await w.string(this.newSessionUri);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _GoAway.#decode);
  }
  static async #decode(r) {
    const newSessionUri = await r.string();
    return new _GoAway({ newSessionUri });
  }
};

// ../lite/src/ietf/namespace.ts
async function encode4(w, namespace) {
  const parts = namespace.split("/");
  await w.u53(parts.length);
  for (const part of parts) {
    await w.string(part);
  }
}
async function decode4(r) {
  const parts = [];
  const count = await r.u53();
  for (let i = 0; i < count; i++) {
    parts.push(await r.string());
  }
  return from(...parts);
}

// ../lite/src/ietf/version.ts
var Version = {
  /**
   * draft-ietf-moq-transport-07
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-07.txt
   */
  DRAFT_07: 4278190087,
  /**
   * draft-ietf-moq-transport-14
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-14.txt
   */
  DRAFT_14: 4278190094,
  /**
   * draft-ietf-moq-transport-15
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-15.txt
   */
  DRAFT_15: 4278190095,
  /**
   * draft-ietf-moq-transport-16
   * https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.txt
   */
  DRAFT_16: 4278190096
};

// ../lite/src/ietf/parameters.ts
var Parameters = class _Parameters {
  vars;
  bytes;
  constructor() {
    this.vars = /* @__PURE__ */ new Map();
    this.bytes = /* @__PURE__ */ new Map();
  }
  get size() {
    return this.vars.size + this.bytes.size;
  }
  setBytes(id, value) {
    if (id % 2n !== 1n) {
      throw new Error(\`invalid parameter id: \${id.toString()}, must be odd\`);
    }
    this.bytes.set(id, value);
  }
  setVarint(id, value) {
    if (id % 2n !== 0n) {
      throw new Error(\`invalid parameter id: \${id.toString()}, must be even\`);
    }
    this.vars.set(id, value);
  }
  getBytes(id) {
    if (id % 2n !== 1n) {
      throw new Error(\`invalid parameter id: \${id.toString()}, must be odd\`);
    }
    return this.bytes.get(id);
  }
  getVarint(id) {
    if (id % 2n !== 0n) {
      throw new Error(\`invalid parameter id: \${id.toString()}, must be even\`);
    }
    return this.vars.get(id);
  }
  removeBytes(id) {
    if (id % 2n !== 1n) {
      throw new Error(\`invalid parameter id: \${id.toString()}, must be odd\`);
    }
    return this.bytes.delete(id);
  }
  removeVarint(id) {
    if (id % 2n !== 0n) {
      throw new Error(\`invalid parameter id: \${id.toString()}, must be even\`);
    }
    return this.vars.delete(id);
  }
  async encode(w, version) {
    await w.u53(this.vars.size + this.bytes.size);
    if (version === Version.DRAFT_16) {
      const all = [];
      for (const id of this.vars.keys()) all.push({ key: id, isVar: true });
      for (const id of this.bytes.keys()) all.push({ key: id, isVar: false });
      all.sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
      let prevId = 0n;
      for (let i = 0; i < all.length; i++) {
        const { key, isVar } = all[i];
        const delta = i === 0 ? key : key - prevId;
        prevId = key;
        await w.u62(delta);
        if (isVar) {
          await w.u62(this.vars.get(key));
        } else {
          const value = this.bytes.get(key);
          await w.u53(value.length);
          await w.write(value);
        }
      }
    } else {
      for (const [id, value] of this.vars) {
        await w.u62(id);
        await w.u62(value);
      }
      for (const [id, value] of this.bytes) {
        await w.u62(id);
        await w.u53(value.length);
        await w.write(value);
      }
    }
  }
  static async decode(r, version) {
    const count = await r.u53();
    const params = new _Parameters();
    let prevType = 0n;
    for (let i = 0; i < count; i++) {
      let id;
      if (version === Version.DRAFT_16) {
        const delta = await r.u62();
        id = i === 0 ? delta : prevType + delta;
        prevType = id;
      } else {
        id = await r.u62();
      }
      if (id % 2n === 0n) {
        if (params.vars.has(id)) {
          throw new Error(\`duplicate parameter id: \${id.toString()}\`);
        }
        const varint = await r.u62();
        params.setVarint(id, varint);
      } else {
        if (params.bytes.has(id)) {
          throw new Error(\`duplicate parameter id: \${id.toString()}\`);
        }
        const size = await r.u53();
        const bytes = await r.read(size);
        params.setBytes(id, bytes);
      }
    }
    return params;
  }
};
var MSG_PARAM_DELIVERY_TIMEOUT = 0x02n;
var MSG_PARAM_MAX_CACHE_DURATION = 0x04n;
var MSG_PARAM_EXPIRES = 0x08n;
var MSG_PARAM_PUBLISHER_PRIORITY = 0x0en;
var MSG_PARAM_FORWARD = 0x10n;
var MSG_PARAM_SUBSCRIBER_PRIORITY = 0x20n;
var MSG_PARAM_GROUP_ORDER = 0x22n;
var MSG_PARAM_LARGEST_OBJECT = 0x09n;
var MSG_PARAM_SUBSCRIPTION_FILTER = 0x21n;
var MessageParameters = class _MessageParameters {
  vars;
  bytes;
  constructor() {
    this.vars = /* @__PURE__ */ new Map();
    this.bytes = /* @__PURE__ */ new Map();
  }
  // --- Varint accessors ---
  get subscriberPriority() {
    const v = this.vars.get(MSG_PARAM_SUBSCRIBER_PRIORITY);
    return v !== void 0 ? Number(v) : void 0;
  }
  set subscriberPriority(v) {
    this.vars.set(MSG_PARAM_SUBSCRIBER_PRIORITY, BigInt(v));
  }
  get groupOrder() {
    const v = this.vars.get(MSG_PARAM_GROUP_ORDER);
    return v !== void 0 ? Number(v) : void 0;
  }
  set groupOrder(v) {
    this.vars.set(MSG_PARAM_GROUP_ORDER, BigInt(v));
  }
  get forward() {
    const v = this.vars.get(MSG_PARAM_FORWARD);
    return v !== void 0 ? v !== 0n : void 0;
  }
  set forward(v) {
    this.vars.set(MSG_PARAM_FORWARD, v ? 1n : 0n);
  }
  get publisherPriority() {
    const v = this.vars.get(MSG_PARAM_PUBLISHER_PRIORITY);
    return v !== void 0 ? Number(v) : void 0;
  }
  set publisherPriority(v) {
    this.vars.set(MSG_PARAM_PUBLISHER_PRIORITY, BigInt(v));
  }
  get expires() {
    return this.vars.get(MSG_PARAM_EXPIRES);
  }
  set expires(v) {
    this.vars.set(MSG_PARAM_EXPIRES, v);
  }
  get deliveryTimeout() {
    return this.vars.get(MSG_PARAM_DELIVERY_TIMEOUT);
  }
  set deliveryTimeout(v) {
    this.vars.set(MSG_PARAM_DELIVERY_TIMEOUT, v);
  }
  get maxCacheDuration() {
    return this.vars.get(MSG_PARAM_MAX_CACHE_DURATION);
  }
  set maxCacheDuration(v) {
    this.vars.set(MSG_PARAM_MAX_CACHE_DURATION, v);
  }
  // --- Bytes accessors ---
  get largest() {
    const data = this.bytes.get(MSG_PARAM_LARGEST_OBJECT);
    if (!data || data.length === 0) return void 0;
    const [groupId, rest] = decode2(data);
    const [objectId] = decode2(rest);
    return { groupId: BigInt(groupId), objectId: BigInt(objectId) };
  }
  set largest(v) {
    const buf1 = encode2(Number(v.groupId));
    const buf2 = encode2(Number(v.objectId));
    const combined = new Uint8Array(buf1.length + buf2.length);
    combined.set(buf1, 0);
    combined.set(buf2, buf1.length);
    this.bytes.set(MSG_PARAM_LARGEST_OBJECT, combined);
  }
  get subscriptionFilter() {
    const data = this.bytes.get(MSG_PARAM_SUBSCRIPTION_FILTER);
    if (!data || data.length === 0) return void 0;
    return data[0];
  }
  set subscriptionFilter(v) {
    this.bytes.set(MSG_PARAM_SUBSCRIPTION_FILTER, new Uint8Array([v]));
  }
  async encode(w, version) {
    await w.u53(this.vars.size + this.bytes.size);
    if (version === Version.DRAFT_16) {
      const all = [];
      for (const id of this.vars.keys()) all.push({ key: id, isVar: true });
      for (const id of this.bytes.keys()) all.push({ key: id, isVar: false });
      all.sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
      let prevId = 0n;
      for (let i = 0; i < all.length; i++) {
        const { key, isVar } = all[i];
        const delta = i === 0 ? key : key - prevId;
        prevId = key;
        await w.u62(delta);
        if (isVar) {
          await w.u62(this.vars.get(key));
        } else {
          const value = this.bytes.get(key);
          await w.u53(value.length);
          await w.write(value);
        }
      }
    } else {
      for (const [id, value] of this.vars) {
        await w.u62(id);
        await w.u62(value);
      }
      for (const [id, value] of this.bytes) {
        await w.u62(id);
        await w.u53(value.length);
        await w.write(value);
      }
    }
  }
  static async decode(r, version) {
    const count = await r.u53();
    const params = new _MessageParameters();
    let prevType = 0n;
    for (let i = 0; i < count; i++) {
      let id;
      if (version === Version.DRAFT_16) {
        const delta = await r.u62();
        id = i === 0 ? delta : prevType + delta;
        prevType = id;
      } else {
        id = await r.u62();
      }
      if (id % 2n === 0n) {
        if (params.vars.has(id)) {
          throw new Error(\`duplicate message parameter id: \${id.toString()}\`);
        }
        const varint = await r.u62();
        params.vars.set(id, varint);
      } else {
        if (params.bytes.has(id)) {
          throw new Error(\`duplicate message parameter id: \${id.toString()}\`);
        }
        const size = await r.u53();
        const bytes = await r.read(size);
        params.bytes.set(id, bytes);
      }
    }
    return params;
  }
};

// ../lite/src/ietf/publish.ts
var Publish = class _Publish {
  static id = 29;
  requestId;
  trackNamespace;
  trackName;
  trackAlias;
  groupOrder;
  contentExists;
  largest;
  forward;
  constructor({
    requestId,
    trackNamespace,
    trackName,
    trackAlias,
    groupOrder,
    contentExists,
    largest,
    forward
  }) {
    this.requestId = requestId;
    this.trackNamespace = trackNamespace;
    this.trackName = trackName;
    this.trackAlias = trackAlias;
    this.groupOrder = groupOrder;
    this.contentExists = contentExists;
    this.largest = largest;
    this.forward = forward;
  }
  async #encode(w, version) {
    await w.u62(this.requestId);
    await encode4(w, this.trackNamespace);
    await w.string(this.trackName);
    await w.u62(this.trackAlias);
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      const params = new MessageParameters();
      params.groupOrder = this.groupOrder;
      params.forward = this.forward;
      if (this.largest) {
        params.largest = this.largest;
      }
      await params.encode(w, version);
    } else if (version === Version.DRAFT_14) {
      await w.u8(this.groupOrder);
      await w.bool(this.contentExists);
      if (this.contentExists !== !!this.largest) {
        throw new Error("contentExists and largest must both be true or false");
      }
      if (this.largest) {
        await w.u62(this.largest.groupId);
        await w.u62(this.largest.objectId);
      }
      await w.bool(this.forward);
      await w.u53(0);
    } else {
      unreachable(version);
    }
  }
  async encode(w, version) {
    return encode3(w, (mw) => this.#encode(mw, version));
  }
  static async decode(r, version) {
    return decode3(r, (mr) => _Publish.#decode(mr, version));
  }
  static async #decode(r, version) {
    const requestId = await r.u62();
    const trackNamespace = await decode4(r);
    const trackName = await r.string();
    const trackAlias = await r.u62();
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      const params = await MessageParameters.decode(r, version);
      const groupOrder = params.groupOrder ?? 2;
      const forward = params.forward ?? true;
      const largest = params.largest;
      return new _Publish({
        requestId,
        trackNamespace,
        trackName,
        trackAlias,
        groupOrder,
        contentExists: !!largest,
        largest,
        forward
      });
    } else if (version === Version.DRAFT_14) {
      const groupOrder = await r.u8();
      const contentExists = await r.bool();
      const largest = contentExists ? { groupId: await r.u62(), objectId: await r.u62() } : void 0;
      const forward = await r.bool();
      await Parameters.decode(r, version);
      return new _Publish({
        requestId,
        trackNamespace,
        trackName,
        trackAlias,
        groupOrder,
        contentExists,
        largest,
        forward
      });
    } else {
      unreachable(version);
    }
  }
};
var PublishOk = class _PublishOk {
  static id = 30;
  async #encode(_w) {
    throw new Error("PUBLISH_OK messages are not supported");
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _PublishOk.#decode);
  }
  static async #decode(_r) {
    throw new Error("PUBLISH_OK messages are not supported");
  }
};
var PublishError = class _PublishError {
  static id = 31;
  requestId;
  errorCode;
  reasonPhrase;
  constructor({
    requestId,
    errorCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w) {
    await w.u62(this.requestId);
    await w.u62(BigInt(this.errorCode));
    await w.string(this.reasonPhrase);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _PublishError.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    const errorCode = Number(await r.u62());
    const reasonPhrase = await r.string();
    return new _PublishError({ requestId, errorCode, reasonPhrase });
  }
};
var PublishDone = class _PublishDone {
  static id = 11;
  requestId;
  statusCode;
  reasonPhrase;
  constructor({
    requestId,
    statusCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.statusCode = statusCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w) {
    await w.u62(this.requestId);
    await w.u62(BigInt(this.statusCode));
    await w.u62(BigInt(0));
    await w.string(this.reasonPhrase);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _PublishDone.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    const statusCode = Number(await r.u62());
    await r.u62();
    const reasonPhrase = await r.string();
    return new _PublishDone({ requestId, statusCode, reasonPhrase });
  }
};

// ../lite/src/ietf/publish_namespace.ts
var PublishNamespace = class _PublishNamespace {
  static id = 6;
  requestId;
  trackNamespace;
  constructor({ requestId, trackNamespace }) {
    this.requestId = requestId;
    this.trackNamespace = trackNamespace;
  }
  async #encode(w, _version) {
    await w.u62(this.requestId);
    await encode4(w, this.trackNamespace);
    await w.u53(0);
  }
  async encode(w, version) {
    return encode3(w, (wr) => this.#encode(wr, version));
  }
  static async decode(r, version) {
    return decode3(r, (rd) => _PublishNamespace.#decode(rd, version));
  }
  static async #decode(r, version) {
    const requestId = await r.u62();
    const trackNamespace = await decode4(r);
    await Parameters.decode(r, version);
    return new _PublishNamespace({ requestId, trackNamespace });
  }
};
var PublishNamespaceOk = class _PublishNamespaceOk {
  static id = 7;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _PublishNamespaceOk.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    return new _PublishNamespaceOk({ requestId });
  }
};
var PublishNamespaceError = class _PublishNamespaceError {
  static id = 8;
  requestId;
  errorCode;
  reasonPhrase;
  constructor({
    requestId,
    errorCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w) {
    await w.u62(this.requestId);
    await w.u62(BigInt(this.errorCode));
    await w.string(this.reasonPhrase);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _PublishNamespaceError.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    const errorCode = Number(await r.u62());
    const reasonPhrase = await r.string();
    return new _PublishNamespaceError({ requestId, errorCode, reasonPhrase });
  }
};
var PublishNamespaceCancel = class _PublishNamespaceCancel {
  static id = 12;
  trackNamespace;
  requestId;
  // v16: uses request_id instead of track_namespace
  errorCode;
  reasonPhrase;
  constructor({
    trackNamespace = "",
    errorCode = 0,
    reasonPhrase = "",
    requestId = 0n
  } = {}) {
    this.trackNamespace = trackNamespace;
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w, version) {
    if (version === Version.DRAFT_16) {
      await w.u62(this.requestId);
    } else {
      await encode4(w, this.trackNamespace);
    }
    await w.u62(BigInt(this.errorCode));
    await w.string(this.reasonPhrase);
  }
  async encode(w, version) {
    return encode3(w, (wr) => this.#encode(wr, version));
  }
  static async decode(r, version) {
    return decode3(r, (rd) => _PublishNamespaceCancel.#decode(rd, version));
  }
  static async #decode(r, version) {
    let trackNamespace = "";
    let requestId = 0n;
    if (version === Version.DRAFT_16) {
      requestId = await r.u62();
    } else {
      trackNamespace = await decode4(r);
    }
    const errorCode = Number(await r.u62());
    const reasonPhrase = await r.string();
    return new _PublishNamespaceCancel({ trackNamespace, errorCode, reasonPhrase, requestId });
  }
};
var PublishNamespaceDone = class _PublishNamespaceDone {
  static id = 9;
  trackNamespace;
  requestId;
  // v16: uses request_id instead of track_namespace
  constructor({
    trackNamespace = "",
    requestId = 0n
  } = {}) {
    this.trackNamespace = trackNamespace;
    this.requestId = requestId;
  }
  async #encode(w, version) {
    if (version === Version.DRAFT_16) {
      await w.u62(this.requestId);
    } else {
      await encode4(w, this.trackNamespace);
    }
  }
  async encode(w, version) {
    return encode3(w, (wr) => this.#encode(wr, version));
  }
  static async decode(r, version) {
    return decode3(r, (rd) => _PublishNamespaceDone.#decode(rd, version));
  }
  static async #decode(r, version) {
    if (version === Version.DRAFT_16) {
      const requestId = await r.u62();
      return new _PublishNamespaceDone({ requestId });
    }
    const trackNamespace = await decode4(r);
    return new _PublishNamespaceDone({ trackNamespace });
  }
};

// ../lite/src/ietf/request.ts
var MaxRequestId = class _MaxRequestId {
  static id = 21;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async #decode(r) {
    return new _MaxRequestId({ requestId: await r.u62() });
  }
  static async decode(r, _version) {
    return decode3(r, _MaxRequestId.#decode);
  }
};
var RequestsBlocked = class _RequestsBlocked {
  static id = 26;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async #decode(r) {
    return new _RequestsBlocked({ requestId: await r.u62() });
  }
  static async decode(r, _version) {
    return decode3(r, _RequestsBlocked.#decode);
  }
};
var RequestOk = class _RequestOk {
  static id = 7;
  requestId;
  parameters;
  constructor({
    requestId,
    parameters = new MessageParameters()
  }) {
    this.requestId = requestId;
    this.parameters = parameters;
  }
  async #encode(w, version) {
    await w.u62(this.requestId);
    await this.parameters.encode(w, version);
  }
  async encode(w, version) {
    return encode3(w, (wr) => this.#encode(wr, version));
  }
  static async #decode(r, version) {
    const requestId = await r.u62();
    const parameters = await MessageParameters.decode(r, version);
    return new _RequestOk({ requestId, parameters });
  }
  static async decode(r, version) {
    return decode3(r, (rd) => _RequestOk.#decode(rd, version));
  }
};
var RequestError = class _RequestError {
  static id = 5;
  requestId;
  errorCode;
  reasonPhrase;
  retryInterval;
  constructor({
    requestId,
    errorCode,
    reasonPhrase,
    retryInterval = 0n
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
    this.retryInterval = retryInterval;
  }
  async #encode(w, version) {
    await w.u62(this.requestId);
    await w.u62(BigInt(this.errorCode));
    if (version === Version.DRAFT_16) {
      await w.u62(this.retryInterval);
    }
    await w.string(this.reasonPhrase);
  }
  async encode(w, version) {
    return encode3(w, (wr) => this.#encode(wr, version));
  }
  static async #decode(r, version) {
    const requestId = await r.u62();
    const errorCode = Number(await r.u62());
    const retryInterval = version === Version.DRAFT_16 ? await r.u62() : 0n;
    const reasonPhrase = await r.string();
    return new _RequestError({ requestId, errorCode, reasonPhrase, retryInterval });
  }
  static async decode(r, version) {
    return decode3(r, (rd) => _RequestError.#decode(rd, version));
  }
};

// ../lite/src/ietf/setup.ts
var MAX_VERSIONS = 128;
var ClientSetup = class _ClientSetup {
  static id = 32;
  versions;
  parameters;
  constructor({ versions, parameters = new Parameters() }) {
    this.versions = versions;
    this.parameters = parameters;
  }
  async #encode(w, version) {
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      await this.parameters.encode(w, version);
    } else if (version === Version.DRAFT_14) {
      await w.u53(this.versions.length);
      for (const v of this.versions) {
        await w.u53(v);
      }
      await this.parameters.encode(w, version);
    } else {
      unreachable(version);
    }
  }
  async encode(w, version) {
    return encode3(w, (mw) => this.#encode(mw, version));
  }
  static async #decode(r, version) {
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      const parameters = await Parameters.decode(r, version);
      return new _ClientSetup({ versions: [version], parameters });
    } else if (version === Version.DRAFT_14) {
      const numVersions = await r.u53();
      if (numVersions > MAX_VERSIONS) {
        throw new Error(\`too many versions: \${numVersions}\`);
      }
      const supportedVersions = [];
      for (let i = 0; i < numVersions; i++) {
        const v = await r.u53();
        supportedVersions.push(v);
      }
      const parameters = await Parameters.decode(r, version);
      return new _ClientSetup({ versions: supportedVersions, parameters });
    } else {
      unreachable(version);
    }
  }
  static async decode(r, version) {
    return decode3(r, (mr) => _ClientSetup.#decode(mr, version));
  }
};
var ServerSetup = class _ServerSetup {
  static id = 33;
  version;
  parameters;
  constructor({ version, parameters = new Parameters() }) {
    this.version = version;
    this.parameters = parameters;
  }
  async #encode(w, version) {
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      await this.parameters.encode(w, version);
    } else if (version === Version.DRAFT_14) {
      await w.u53(this.version);
      await this.parameters.encode(w, version);
    } else {
      unreachable(version);
    }
  }
  async encode(w, version) {
    return encode3(w, (mw) => this.#encode(mw, version));
  }
  static async #decode(r, version) {
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      const parameters = await Parameters.decode(r, version);
      return new _ServerSetup({ version, parameters });
    } else if (version === Version.DRAFT_14) {
      const selectedVersion = await r.u53();
      const parameters = await Parameters.decode(r, version);
      return new _ServerSetup({ version: selectedVersion, parameters });
    } else {
      unreachable(version);
    }
  }
  static async decode(r, version) {
    return decode3(r, (mr) => _ServerSetup.#decode(mr, version));
  }
};

// ../lite/src/ietf/subscribe.ts
var GROUP_ORDER = 2;
var Subscribe = class _Subscribe {
  static id = 3;
  requestId;
  trackNamespace;
  trackName;
  subscriberPriority;
  constructor({
    requestId,
    trackNamespace,
    trackName,
    subscriberPriority
  }) {
    this.requestId = requestId;
    this.trackNamespace = trackNamespace;
    this.trackName = trackName;
    this.subscriberPriority = subscriberPriority;
  }
  async #encode(w, version) {
    await w.u62(this.requestId);
    await encode4(w, this.trackNamespace);
    await w.string(this.trackName);
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      const params = new MessageParameters();
      params.subscriberPriority = this.subscriberPriority;
      params.groupOrder = GROUP_ORDER;
      params.forward = true;
      params.subscriptionFilter = 2;
      await params.encode(w, version);
    } else if (version === Version.DRAFT_14) {
      await w.u8(this.subscriberPriority);
      await w.u8(GROUP_ORDER);
      await w.bool(true);
      await w.u53(2);
      await w.u53(0);
    } else {
      unreachable(version);
    }
  }
  async encode(w, version) {
    return encode3(w, (mw) => this.#encode(mw, version));
  }
  static async decode(r, version) {
    return decode3(r, (mr) => _Subscribe.#decode(mr, version));
  }
  static async #decode(r, version) {
    const requestId = await r.u62();
    const trackNamespace = await decode4(r);
    const trackName = await r.string();
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      const params = await MessageParameters.decode(r, version);
      const subscriberPriority = params.subscriberPriority ?? 128;
      let groupOrder = params.groupOrder ?? GROUP_ORDER;
      if (groupOrder > 2) {
        throw new Error(\`unknown group order: \${groupOrder}\`);
      }
      if (groupOrder === 0) {
        groupOrder = GROUP_ORDER;
      }
      const forward = params.forward ?? true;
      if (!forward) {
        throw new Error(\`unsupported forward value: \${forward}\`);
      }
      const filterType = params.subscriptionFilter ?? 2;
      if (filterType !== 1 && filterType !== 2) {
        throw new Error(\`unsupported filter type: \${filterType}\`);
      }
      return new _Subscribe({ requestId, trackNamespace, trackName, subscriberPriority });
    } else if (version === Version.DRAFT_14) {
      const subscriberPriority = await r.u8();
      let groupOrder = await r.u8();
      if (groupOrder > 2) {
        throw new Error(\`unknown group order: \${groupOrder}\`);
      }
      if (groupOrder === 0) {
        groupOrder = GROUP_ORDER;
      }
      const forward = await r.bool();
      if (!forward) {
        throw new Error(\`unsupported forward value: \${forward}\`);
      }
      const filterType = await r.u53();
      if (filterType !== 1 && filterType !== 2) {
        throw new Error(\`unsupported filter type: \${filterType}\`);
      }
      await Parameters.decode(r, version);
      return new _Subscribe({ requestId, trackNamespace, trackName, subscriberPriority });
    } else {
      unreachable(version);
    }
  }
};
var SubscribeOk = class _SubscribeOk {
  static id = 4;
  requestId;
  trackAlias;
  constructor({ requestId, trackAlias }) {
    this.requestId = requestId;
    this.trackAlias = trackAlias;
  }
  async #encode(w, version) {
    await w.u62(this.requestId);
    await w.u62(this.trackAlias);
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      const params = new MessageParameters();
      params.groupOrder = GROUP_ORDER;
      await params.encode(w, version);
    } else if (version === Version.DRAFT_14) {
      await w.u62(0n);
      await w.u8(GROUP_ORDER);
      await w.bool(false);
      await w.u53(0);
    } else {
      unreachable(version);
    }
  }
  async encode(w, version) {
    return encode3(w, (mw) => this.#encode(mw, version));
  }
  static async decode(r, version) {
    return decode3(r, (mr) => _SubscribeOk.#decode(mr, version));
  }
  static async #decode(r, version) {
    const requestId = await r.u62();
    const trackAlias = await r.u62();
    if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
      await MessageParameters.decode(r, version);
    } else if (version === Version.DRAFT_14) {
      const expires = await r.u62();
      if (expires !== BigInt(0)) {
        throw new Error(\`unsupported expires: \${expires}\`);
      }
      await r.u8();
      const contentExists = await r.bool();
      if (contentExists) {
        await r.u62();
        await r.u62();
      }
      await Parameters.decode(r, version);
    } else {
      unreachable(version);
    }
    return new _SubscribeOk({ requestId, trackAlias });
  }
};
var SubscribeError = class _SubscribeError {
  static id = 5;
  requestId;
  errorCode;
  reasonPhrase;
  constructor({
    requestId,
    errorCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w) {
    await w.u62(this.requestId);
    await w.u62(BigInt(this.errorCode));
    await w.string(this.reasonPhrase);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _SubscribeError.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    const errorCode = Number(await r.u62());
    const reasonPhrase = await r.string();
    return new _SubscribeError({ requestId, errorCode, reasonPhrase });
  }
};
var Unsubscribe = class _Unsubscribe {
  static id = 10;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _Unsubscribe.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    return new _Unsubscribe({ requestId });
  }
};

// ../lite/src/ietf/subscribe_namespace.ts
var SubscribeNamespace = class _SubscribeNamespace {
  static id = 17;
  namespace;
  requestId;
  subscribeOptions;
  // v16: default 0x01 (NAMESPACE only)
  constructor({
    namespace,
    requestId,
    subscribeOptions = 1
  }) {
    this.namespace = namespace;
    this.requestId = requestId;
    this.subscribeOptions = subscribeOptions;
  }
  async #encode(w, version) {
    await w.u62(this.requestId);
    await encode4(w, this.namespace);
    if (version === Version.DRAFT_16) {
      await w.u53(this.subscribeOptions);
    }
    await w.u53(0);
  }
  async encode(w, version) {
    return encode3(w, (wr) => this.#encode(wr, version));
  }
  static async decode(r, version) {
    return decode3(r, (rd) => _SubscribeNamespace.#decode(rd, version));
  }
  static async #decode(r, version) {
    const requestId = await r.u62();
    const namespace = await decode4(r);
    let subscribeOptions = 1;
    if (version === Version.DRAFT_16) {
      subscribeOptions = await r.u53();
    }
    await Parameters.decode(r, version);
    return new _SubscribeNamespace({ namespace, requestId, subscribeOptions });
  }
};
var SubscribeNamespaceOk = class _SubscribeNamespaceOk {
  static id = 18;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _SubscribeNamespaceOk.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    return new _SubscribeNamespaceOk({ requestId });
  }
};
var SubscribeNamespaceError = class _SubscribeNamespaceError {
  static id = 19;
  requestId;
  errorCode;
  reasonPhrase;
  constructor({
    requestId,
    errorCode,
    reasonPhrase
  }) {
    this.requestId = requestId;
    this.errorCode = errorCode;
    this.reasonPhrase = reasonPhrase;
  }
  async #encode(w) {
    await w.u62(this.requestId);
    await w.u62(BigInt(this.errorCode));
    await w.string(this.reasonPhrase);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _SubscribeNamespaceError.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    const errorCode = Number(await r.u62());
    const reasonPhrase = await r.string();
    return new _SubscribeNamespaceError({ requestId, errorCode, reasonPhrase });
  }
};
var UnsubscribeNamespace = class _UnsubscribeNamespace {
  static id = 20;
  requestId;
  constructor({ requestId }) {
    this.requestId = requestId;
  }
  async #encode(w) {
    await w.u62(this.requestId);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _UnsubscribeNamespace.#decode);
  }
  static async #decode(r) {
    const requestId = await r.u62();
    return new _UnsubscribeNamespace({ requestId });
  }
};

// ../lite/src/ietf/track.ts
var TrackStatusRequest = class _TrackStatusRequest {
  static id = 13;
  trackNamespace;
  trackName;
  constructor({ trackNamespace, trackName }) {
    this.trackNamespace = trackNamespace;
    this.trackName = trackName;
  }
  async #encode(w) {
    await encode4(w, this.trackNamespace);
    await w.string(this.trackName);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _TrackStatusRequest.#decode);
  }
  static async #decode(r) {
    const trackNamespace = await decode4(r);
    const trackName = await r.string();
    return new _TrackStatusRequest({ trackNamespace, trackName });
  }
};
var TrackStatus = class _TrackStatus {
  static id = 14;
  trackNamespace;
  trackName;
  statusCode;
  lastGroupId;
  lastObjectId;
  constructor({
    trackNamespace,
    trackName,
    statusCode,
    lastGroupId,
    lastObjectId
  }) {
    this.trackNamespace = trackNamespace;
    this.trackName = trackName;
    this.statusCode = statusCode;
    this.lastGroupId = lastGroupId;
    this.lastObjectId = lastObjectId;
  }
  async #encode(w) {
    await encode4(w, this.trackNamespace);
    await w.string(this.trackName);
    await w.u62(BigInt(this.statusCode));
    await w.u62(this.lastGroupId);
    await w.u62(this.lastObjectId);
  }
  async encode(w, _version) {
    return encode3(w, this.#encode.bind(this));
  }
  static async decode(r, _version) {
    return decode3(r, _TrackStatus.#decode);
  }
  static async #decode(r) {
    const trackNamespace = await decode4(r);
    const trackName = await r.string();
    const statusCode = Number(await r.u62());
    const lastGroupId = await r.u62();
    const lastObjectId = await r.u62();
    return new _TrackStatus({ trackNamespace, trackName, statusCode, lastGroupId, lastObjectId });
  }
  // Track status codes
  static STATUS_IN_PROGRESS = 0;
  static STATUS_NOT_FOUND = 1;
  static STATUS_NOT_AUTHORIZED = 2;
  static STATUS_ENDED = 3;
};

// ../lite/src/ietf/control.ts
var MessagesV14 = {
  [ClientSetup.id]: ClientSetup,
  [ServerSetup.id]: ServerSetup,
  [Subscribe.id]: Subscribe,
  [SubscribeOk.id]: SubscribeOk,
  [SubscribeError.id]: SubscribeError,
  [PublishNamespace.id]: PublishNamespace,
  [PublishNamespaceOk.id]: PublishNamespaceOk,
  [PublishNamespaceError.id]: PublishNamespaceError,
  [PublishNamespaceDone.id]: PublishNamespaceDone,
  [Unsubscribe.id]: Unsubscribe,
  [PublishDone.id]: PublishDone,
  [PublishNamespaceCancel.id]: PublishNamespaceCancel,
  [TrackStatusRequest.id]: TrackStatusRequest,
  [TrackStatus.id]: TrackStatus,
  [GoAway.id]: GoAway,
  [Fetch.id]: Fetch,
  [FetchCancel.id]: FetchCancel,
  [FetchOk.id]: FetchOk,
  [FetchError.id]: FetchError,
  [SubscribeNamespace.id]: SubscribeNamespace,
  [SubscribeNamespaceOk.id]: SubscribeNamespaceOk,
  [SubscribeNamespaceError.id]: SubscribeNamespaceError,
  [UnsubscribeNamespace.id]: UnsubscribeNamespace,
  [Publish.id]: Publish,
  [PublishOk.id]: PublishOk,
  [PublishError.id]: PublishError,
  [MaxRequestId.id]: MaxRequestId,
  [RequestsBlocked.id]: RequestsBlocked
};
var MessagesV15 = {
  [ClientSetup.id]: ClientSetup,
  [ServerSetup.id]: ServerSetup,
  [Subscribe.id]: Subscribe,
  [SubscribeOk.id]: SubscribeOk,
  [RequestError.id]: RequestError,
  // 0x05 → RequestError instead of SubscribeError
  [PublishNamespace.id]: PublishNamespace,
  [RequestOk.id]: RequestOk,
  // 0x07 → RequestOk instead of PublishNamespaceOk
  [PublishNamespaceDone.id]: PublishNamespaceDone,
  [Unsubscribe.id]: Unsubscribe,
  [PublishDone.id]: PublishDone,
  [PublishNamespaceCancel.id]: PublishNamespaceCancel,
  [TrackStatusRequest.id]: TrackStatusRequest,
  [GoAway.id]: GoAway,
  [Fetch.id]: Fetch,
  [FetchCancel.id]: FetchCancel,
  [FetchOk.id]: FetchOk,
  [SubscribeNamespace.id]: SubscribeNamespace,
  [UnsubscribeNamespace.id]: UnsubscribeNamespace,
  [Publish.id]: Publish,
  [MaxRequestId.id]: MaxRequestId,
  [RequestsBlocked.id]: RequestsBlocked
};
var MessagesV16 = {
  [ClientSetup.id]: ClientSetup,
  [ServerSetup.id]: ServerSetup,
  [Subscribe.id]: Subscribe,
  [SubscribeOk.id]: SubscribeOk,
  [RequestError.id]: RequestError,
  // 0x05 → RequestError
  [PublishNamespace.id]: PublishNamespace,
  [RequestOk.id]: RequestOk,
  // 0x07 → RequestOk
  [PublishNamespaceDone.id]: PublishNamespaceDone,
  [Unsubscribe.id]: Unsubscribe,
  [PublishDone.id]: PublishDone,
  [PublishNamespaceCancel.id]: PublishNamespaceCancel,
  [TrackStatusRequest.id]: TrackStatusRequest,
  [GoAway.id]: GoAway,
  [Fetch.id]: Fetch,
  [FetchCancel.id]: FetchCancel,
  [FetchOk.id]: FetchOk,
  // SubscribeNamespace (0x11) removed — now on bidi stream
  // UnsubscribeNamespace (0x14) removed — now use stream close
  [Publish.id]: Publish,
  [MaxRequestId.id]: MaxRequestId,
  [RequestsBlocked.id]: RequestsBlocked
};

// ../lite/src/time.ts
var time_exports = {};
__export(time_exports, {
  Micro: () => Micro,
  Milli: () => Milli,
  Nano: () => Nano,
  Second: () => Second
});
var Nano = {
  zero: 0,
  fromMicro: (us) => us * 1e3,
  fromMilli: (ms) => ms * 1e6,
  fromSecond: (s) => s * 1e9,
  toMicro: (ns) => ns / 1e3,
  toMilli: (ns) => ns / 1e6,
  toSecond: (ns) => ns / 1e9,
  now: () => performance.now() * 1e6,
  add: (a, b) => a + b,
  sub: (a, b) => a - b,
  mul: (a, b) => a * b,
  div: (a, b) => a / b,
  max: (a, b) => Math.max(a, b),
  min: (a, b) => Math.min(a, b)
};
var Micro = {
  zero: 0,
  fromNano: (ns) => ns / 1e3,
  fromMilli: (ms) => ms * 1e3,
  fromSecond: (s) => s * 1e6,
  toNano: (us) => us * 1e3,
  toMilli: (us) => us / 1e3,
  toSecond: (us) => us / 1e6,
  now: () => performance.now() * 1e3,
  add: (a, b) => a + b,
  sub: (a, b) => a - b,
  mul: (a, b) => a * b,
  div: (a, b) => a / b,
  max: (a, b) => Math.max(a, b),
  min: (a, b) => Math.min(a, b)
};
var Milli = {
  zero: 0,
  fromNano: (ns) => ns / 1e6,
  fromMicro: (us) => us / 1e3,
  fromSecond: (s) => s * 1e3,
  toNano: (ms) => ms * 1e6,
  toMicro: (ms) => ms * 1e3,
  toSecond: (ms) => ms / 1e3,
  now: () => performance.now(),
  add: (a, b) => a + b,
  sub: (a, b) => a - b,
  mul: (a, b) => a * b,
  div: (a, b) => a / b,
  max: (a, b) => Math.max(a, b),
  min: (a, b) => Math.min(a, b)
};
var Second = {
  zero: 0,
  fromNano: (ns) => ns / 1e9,
  fromMicro: (us) => us / 1e6,
  fromMilli: (ms) => ms / 1e3,
  toNano: (s) => s * 1e9,
  toMicro: (s) => s * 1e6,
  toMilli: (s) => s * 1e3,
  now: () => performance.now() / 1e3,
  add: (a, b) => a + b,
  sub: (a, b) => a - b,
  mul: (a, b) => a * b,
  div: (a, b) => a / b,
  max: (a, b) => Math.max(a, b),
  min: (a, b) => Math.min(a, b)
};

// src/audio/capture-worklet.ts
var Capture = class extends AudioWorkletProcessor {
  #sampleCount = 0;
  process(input) {
    if (input.length > 1) throw new Error("only one input is supported.");
    const channels = input[0];
    if (channels.length === 0) return true;
    const timestamp = time_exports.Micro.fromSecond(this.#sampleCount / sampleRate);
    const msg = {
      timestamp,
      channels
    };
    this.port.postMessage(msg);
    this.#sampleCount += channels[0].length;
    return true;
  }
};
registerProcessor("capture", Capture);
`,ro=new Blob([so],{type:"application/javascript"}),io=URL.createObjectURL(ro),ht=.001,Ie=.2;let ft=class Wt{static TRACK="audio/data";static PRIORITY=L.audio;enabled;muted;volume;groupDuration;source;#e=new h(void 0);catalog=this.#e;#t=new h(void 0);config=this.#t;#s=new h(void 0);#r=new h(void 0);root=this.#r;active=new h(!1);#i=new S;constructor(t){this.source=h.from(t?.source),this.enabled=h.from(t?.enabled??!1),this.muted=h.from(t?.muted??!1),this.volume=h.from(t?.volume??1),this.groupDuration=t?.groupDuration??100,this.#i.run(this.#n.bind(this)),this.#i.run(this.#a.bind(this)),this.#i.run(this.#o.bind(this)),this.#i.run(this.#c.bind(this))}#n(t){const s=t.getAll([this.enabled,this.source]);if(!s)return;const[r,i]=s,n=i.getSettings(),a=new AudioContext({latencyHint:"interactive",sampleRate:n.sampleRate});t.cleanup(()=>a.close());const o=new MediaStreamAudioSourceNode(a,{mediaStream:new MediaStream([i])});t.cleanup(()=>o.disconnect());const c=new GainNode(a,{gain:this.volume.peek()});o.connect(c),t.cleanup(()=>c.disconnect()),t.spawn(async()=>{if(await a.audioWorklet.addModule(io),a.state==="closed")return;const u=new AudioWorkletNode(a,"capture",{numberOfInputs:1,numberOfOutputs:0,channelCount:n.channelCount??o.channelCount});t.set(this.#s,u),c.connect(u),t.cleanup(()=>u.disconnect()),t.set(this.#r,c)})}#a(t){const s=t.getAll([this.source,this.#s]);if(!s)return;const[r,i]=s,n={codec:"opus",sampleRate:M(i.context.sampleRate),numberOfChannels:M(i.channelCount),bitrate:M(i.channelCount*32e3),container:{kind:"legacy"}};t.set(this.#t,n)}#o(t){const s=t.get(this.#r);if(!s)return;t.cleanup(()=>s.gain.cancelScheduledValues(s.context.currentTime));const r=t.get(this.muted)?0:t.get(this.volume);r<ht?(s.gain.exponentialRampToValueAtTime(ht,s.context.currentTime+Ie),s.gain.setValueAtTime(0,s.context.currentTime+Ie+.01)):s.gain.exponentialRampToValueAtTime(r,s.context.currentTime+Ie)}serve(t,s){const r=s.getAll([this.enabled,this.#s,this.#t]);if(!r)return;const[i,n,a]=r;s.set(this.active,!0,!1);const o=new me(t);s.cleanup(()=>o.close());let c;s.spawn(async()=>{await to();const u=new AudioEncoder({output:l=>{if(l.type!=="key")throw new Error("only key frames are supported");let f=!1;(!c||c+Ae.fromMilli(this.groupDuration)<=l.timestamp)&&(c=l.timestamp,f=!0),o.encode(l,l.timestamp,f)},error:l=>{console.error("encoder error",l),o.close(l),n.port.onmessage=null}});s.cleanup(()=>u.close()),console.debug("encoding audio",a),u.configure(a),n.port.onmessage=({data:l})=>{const f=l.channels.slice(0,n.channelCount),m=f.reduce((N,B)=>N+B.length,0),w=new Float32Array(m);f.reduce((N,B)=>(w.set(B,N),N+B.length),0);const v=new AudioData({format:"f32-planar",sampleRate:n.context.sampleRate,numberOfFrames:f[0].length,numberOfChannels:f.length,timestamp:l.timestamp,data:w,transfer:[w.buffer]});u.encode(v),v.close()},s.cleanup(()=>{n.port.onmessage=null})})}#c(t){const s=t.get(this.#t);if(!s)return;const r={renditions:{[Wt.TRACK]:s}};t.set(this.#e,r)}close(){this.#i.close()}};class we{static TRACK="chat/message.txt";static PRIORITY=L.chat;enabled;latest;catalog=new h(void 0);#e=new S;constructor(t){this.enabled=h.from(t?.enabled??!1),this.latest=new h(""),this.#e.run(s=>{s.get(this.enabled)&&s.set(this.catalog,{name:we.TRACK})})}serve(t,s){if(!s.get(this.enabled))return;const r=s.get(this.latest);t.writeString(r??"")}close(){this.#e.close()}}class ve{static TRACK="chat/typing.bool";static PRIORITY=L.typing;enabled;active;catalog=new h(void 0);#e=new S;constructor(t){this.enabled=h.from(t?.enabled??!1),this.active=new h(!1),this.#e.run(s=>{s.get(this.enabled)&&s.set(this.catalog,{name:ve.TRACK})})}serve(t,s){if(!s.get(this.enabled))return;const r=s.get(this.active);t.writeBool(r)}close(){this.#e.close()}}let no=class{message;typing;#e=new h(void 0);catalog=this.#e;#t=new S;constructor(e){this.message=new we(e?.message),this.typing=new ve(e?.typing),this.#t.run(t=>{this.#e.set({message:t.get(this.message.catalog),typing:t.get(this.typing.catalog)})})}close(){this.#t.close(),this.message.close(),this.typing.close()}};function Kt(e,t,s){const r=s.parse(t);e.writeJson(r)}class be{static TRACK="location/peers.json";static PRIORITY=L.location;enabled;positions=new h({});catalog=new h(void 0);signals=new S;constructor(t){this.enabled=h.from(t?.enabled??!1),this.positions=h.from(t?.positions??{}),this.signals.run(s=>{s.get(this.enabled)&&s.set(this.catalog,{name:be.TRACK})})}serve(t,s){const r=s.getAll([this.enabled,this.positions]);if(!r)return;const[i,n]=r;Kt(t,n,Xa)}close(){this.signals.close()}}class ge{static TRACK="location/window.json";static PRIORITY=L.location;enabled;position;handle;catalog=new h(void 0);signals=new S;constructor(t){this.enabled=h.from(t?.enabled??!1),this.position=h.from(t?.position??void 0),this.handle=h.from(t?.handle??void 0),this.signals.run(s=>{s.get(this.enabled)&&s.set(this.catalog,{initial:this.position.peek(),track:{name:ge.TRACK},handle:s.get(this.handle)})})}serve(t,s){const r=s.getAll([this.enabled,this.position]);if(!r)return;const[i,n]=r;Kt(t,n,$e)}close(){this.signals.close()}}let ao=class{window;peers;catalog=new h(void 0);signals=new S;constructor(e){this.window=new ge(e?.window),this.peers=new be(e?.peers),this.signals.run(this.#e.bind(this))}#e(e){const t=e.get(this.window.catalog),s=e.get(this.peers.catalog);!t&&!s||e.set(this.catalog,{peers:s,...t})}close(){this.signals.close(),this.window.close(),this.peers.close()}};class de{static TRACK="preview.json";static PRIORITY=L.preview;enabled;info;catalog=new h(void 0);signals=new S;constructor(t){this.enabled=h.from(t?.enabled??!1),this.info=h.from(t?.info),this.signals.run(s=>{s.get(this.enabled)&&s.set(this.catalog,{name:de.TRACK})})}serve(t,s){const r=s.getAll([this.enabled,this.info]);if(!r)return;const[i,n]=r;t.writeJson(n)}close(){this.signals.close()}}class oo{enabled;id;name;avatar;color;catalog=new h(void 0);signals=new S;constructor(t){this.enabled=h.from(t?.enabled??!1),this.id=h.from(t?.id),this.name=h.from(t?.name),this.avatar=h.from(t?.avatar),this.color=h.from(t?.color),this.signals.run(s=>{s.get(this.enabled)&&s.set(this.catalog,{id:s.get(this.id),name:s.get(this.name),avatar:s.get(this.avatar),color:s.get(this.color)})})}close(){this.signals.close()}}class pt{enabled;source;frame;#e=new h(void 0);catalog=this.#e;#t=new S;config;#s=new h(void 0);#r=new h(void 0);active=new h(!1);constructor(t,s,r){this.frame=t,this.source=s,this.enabled=h.from(r?.enabled??!1),this.config=h.from(r?.config),this.#t.run(this.#i.bind(this)),this.#t.run(this.#n.bind(this)),this.#t.run(this.#a.bind(this))}serve(t,s){if(!s.get(this.enabled))return;const r=new me(t);s.cleanup(()=>r.close());let i;s.set(this.active,!0,!1),s.spawn(async()=>{const n=new VideoEncoder({output:a=>{a.type==="key"&&(i=a.timestamp),r.encode(a,a.timestamp,a.type==="key")},error:a=>{r.close(a)}});s.cleanup(()=>n.close()),s.run(()=>{const a=s.get(this.#r);a&&n.configure(a)}),s.run(a=>{const o=a.get(this.frame);if(!o||n.state!=="configured")return;const c=this.config.peek()?.keyframeInterval??ae.fromSecond(2),u=!i||i+Ae.fromMilli(c)<=o.timestamp;u&&(i=o.timestamp),n.encode(o,{keyFrame:u})})})}#i(t){const s=t.getAll([this.enabled,this.#r]);if(!s)return;const[r,i]=s,n={codec:i.codec,bitrate:i.bitrate?M(i.bitrate):void 0,framerate:i.framerate,codedWidth:M(i.width),codedHeight:M(i.height),optimizeForLatency:!0,container:{kind:"legacy"}};t.set(this.#e,n)}#n(t){const s=t.getAll([this.enabled,this.source,this.#s]);if(!s)return;const[r,i,n]=s,a=i.getSettings().frameRate??30,o=t.get(this.config)??{},c=o.maxPixels??n.width*n.height,u=o.bitrateScale??.07;t.spawn(async()=>{const l=await this.#o(t);if(!l)return;const{codec:f,hardwareAcceleration:m}=l,w=30+(a-30)/2;let v=Math.round(c*u*w);if(f.startsWith("avc1"))v*=1;else if(f.startsWith("hev1"))v*=.7;else if(f.startsWith("vp09"))v*=.8;else if(f.startsWith("av01"))v*=.6;else if(f==="vp8")v*=1.1;else throw new Error(`unknown codec: ${f}`);v=Math.round(Math.min(v,o.maxBitrate||v));const N={codec:f,width:n.width,height:n.height,framerate:a,bitrate:v,avc:f.startsWith("avc1")?{format:"annexb"}:void 0,hevc:f.startsWith("hev1")?{format:"annexb"}:void 0,latencyMode:"realtime",hardwareAcceleration:m};t.set(this.#r,N)})}#a(t){const s=t.get(this.config),r=t.get(this.frame);if(!r)return;const i=s?.maxPixels??r.codedWidth*r.codedHeight,n=Math.min(Math.sqrt(i/(r.codedWidth*r.codedHeight)),1),a=16*Math.floor(r.codedWidth*n/16),o=16*Math.floor(r.codedHeight*n/16);t.set(this.#s,{width:a,height:o})}async#o(t){const s=t.get(this.config)?.codec??"",r=t.get(this.#s);if(!r)return;const i=["vp09.00.10.08","vp09","avc1.640028","avc1.4D401F","avc1.42E01E","avc1","av01.0.08M.08","av01","hev1.1.6.L93.B0","hev1","vp8"],n=["avc1.640028","avc1.4D401F","avc1.42E01E","avc1","vp8","vp09.00.10.08","vp09","hev1.1.6.L93.B0","hev1","av01.0.08M.08","av01"];if(!eo)for(const a of i){if(!a.startsWith(s))continue;const o="prefer-hardware",c={codec:a,width:r.width,height:r.height,latencyMode:"realtime",hardwareAcceleration:o,avc:a.startsWith("avc1")?{format:"annexb"}:void 0,hevc:a.startsWith("hev1")?{format:"annexb"}:void 0},{supported:u}=await VideoEncoder.isConfigSupported(c);if(u)return{codec:a,hardwareAcceleration:o}}for(const a of n){if(!a.startsWith(s))continue;const o="prefer-software",c={codec:a,width:r.width,height:r.height,latencyMode:"realtime",hardwareAcceleration:o,avc:a.startsWith("avc1")?{format:"annexb"}:void 0,hevc:a.startsWith("hev1")?{format:"annexb"}:void 0},{supported:u}=await VideoEncoder.isConfigSupported(c);if(u)return{codec:a,hardwareAcceleration:o}}throw new Error("no supported codec")}close(){this.#t.close()}}function co(e){if(self.MediaStreamTrackProcessor){const n=performance.now()*1e3,a=new TransformStream({transform(o,c){const u=new VideoFrame(o,{timestamp:o.timestamp+n});o.close(),c.enqueue(u)}});return new self.MediaStreamTrackProcessor({track:e}).readable.pipeThrough(a)}console.warn("Using MediaStreamTrackProcessor polyfill; performance might suffer.");const t=e.getSettings();if(!t)throw new Error("track has no settings");let s,r;const i=t.frameRate??30;return new ReadableStream({async start(){s=document.createElement("video"),s.srcObject=new MediaStream([e]),await Promise.all([s.play(),new Promise(n=>{s.onloadedmetadata=n})]),r=ae.now()},async pull(n){for(;;){const a=ae.now();if(ae.sub(a,r)<1e3/i){await new Promise(o=>requestAnimationFrame(o));continue}r=a,n.enqueue(new VideoFrame(s,{timestamp:Ae.fromMilli(r)}))}}})}class K{static TRACK_HD="video/hd";static TRACK_SD="video/sd";static PRIORITY=L.video;source;hd;sd;frame=new h(void 0);catalog=new h(void 0);display=new h(void 0);flip=new h(!1);signals=new S;constructor(t){this.source=h.from(t?.source),this.hd=new pt(this.frame,this.source,t?.hd),this.sd=new pt(this.frame,this.source,t?.sd),this.flip=h.from(t?.flip??!1),this.signals.run(this.#t.bind(this)),this.signals.run(this.#e.bind(this))}#e(t){const s=t.get(this.source);if(!s)return;const r=co(s).getReader();t.cleanup(()=>r.cancel()),t.spawn(async()=>{for(;;){const i=await Promise.race([r.read(),t.cancel]);if(!i||!i.value)break;this.frame.update(n=>(n?.close(),i.value)),this.display.set({width:i.value.codedWidth,height:i.value.codedHeight})}}),t.cleanup(()=>{this.frame.update(i=>{i?.close()}),this.display.set(void 0)})}#t(t){if(!t.get(this.source))return;const s=t.get(this.display);if(!s)return;const r=t.get(this.hd.catalog),i=t.get(this.sd.catalog),n={};r&&(n[K.TRACK_HD]=r),i&&(n[K.TRACK_SD]=i);const a={renditions:n,display:{width:M(s.width),height:M(s.height)},flip:t.get(this.flip)??void 0};t.set(this.catalog,a)}close(){this.signals.close(),this.hd.close(),this.sd.close(),this.frame.update(t=>{t?.close()})}}class Ce{static CATALOG_TRACK="catalog.json";connection;enabled;name;audio;video;location;chat;preview;user;signals=new S;constructor(t){this.connection=h.from(t?.connection),this.enabled=h.from(t?.enabled??!1),this.name=h.from(t?.name??es()),this.audio=new ft(t?.audio),this.video=new K(t?.video),this.location=new ao(t?.location),this.chat=new no(t?.chat),this.preview=new de(t?.preview),this.user=new oo(t?.user),this.signals.run(this.#e.bind(this))}#e(t){const s=t.getAll([this.enabled,this.connection]);if(!s)return;const[r,i]=s,n=t.get(this.name),a=new ts;t.cleanup(()=>a.close()),i.publish(n,a),t.spawn(this.#t.bind(this,a,t))}async#t(t,s){for(;;){const r=await t.requested();if(!r)break;s.cleanup(()=>r.track.close()),s.run(i=>{if(!i.get(r.track.state.closed))switch(r.track.name){case Ce.CATALOG_TRACK:this.#s(r.track,i);break;case ge.TRACK:this.location.window.serve(r.track,i);break;case be.TRACK:this.location.peers.serve(r.track,i);break;case de.TRACK:this.preview.serve(r.track,i);break;case ve.TRACK:this.chat.typing.serve(r.track,i);break;case we.TRACK:this.chat.message.serve(r.track,i);break;case ft.TRACK:this.audio.serve(r.track,i);break;case K.TRACK_HD:this.video.hd.serve(r.track,i);break;case K.TRACK_SD:this.video.sd.serve(r.track,i);break;default:console.error("received subscription for unknown track",r.track.name),r.track.close(new Error(`Unknown track: ${r.track.name}`));break}})}}#s(t,s){if(!s.get(this.enabled)){t.writeFrame(lt({}));return}const r={video:s.get(this.video.catalog),audio:s.get(this.audio.catalog),location:s.get(this.location.catalog),user:s.get(this.user.catalog),chat:s.get(this.chat.catalog),preview:s.get(this.preview.catalog)},i=lt(r);t.writeFrame(i)}close(){this.signals.close(),this.audio.close(),this.video.close(),this.location.close(),this.chat.close(),this.preview.close(),this.user.close()}}class Xt{kind;#e=new h(void 0);available=this.#e;#t=new h(void 0);default=this.#t;preferred;active=new h(void 0);permission=new h(!1);#s=new h(void 0);requested=this.#s;signals=new S;constructor(t,s){this.kind=t,this.preferred=h.from(s?.preferred),this.signals.run(r=>{r.spawn(this.#r.bind(this,r)),r.event(navigator.mediaDevices,"devicechange",()=>this.permission.mutate(()=>{}))}),this.signals.run(this.#i.bind(this))}async#r(t){t.get(this.permission);let s=await Promise.race([navigator.mediaDevices.enumerateDevices().catch(()=>{}),t.cancel]);if(!s)return;if(s=s.filter(n=>n.kind===`${this.kind}input`),s.some(n=>n.deviceId==="")){console.warn(`no ${this.kind} permission`),this.#e.set(void 0),this.#t.set(void 0);return}this.permission.set(!0),s.length||console.warn(`no ${this.kind} devices found`);const r=s.find(n=>n.deviceId==="default");s=s.filter(n=>n.deviceId!=="default");let i;r&&(i=s.find(n=>n.groupId===r.groupId)),i||(this.kind==="audio"?i=s.find(n=>{const a=n.label.toLowerCase();return a.includes("default")||a.includes("communications")}):this.kind==="video"&&(i=s.find(n=>{const a=n.label.toLowerCase();return a.includes("front")||a.includes("external")||a.includes("usb")}))),i||(i=s.at(0)),this.#e.set(s),this.#t.set(i?.deviceId)}#i(t){const s=t.get(this.preferred);s&&t.get(this.#e)?.some(r=>r.deviceId===s)?this.#s.set(s):this.#s.set(t.get(this.default))}requestPermission(){this.permission.peek()||navigator.mediaDevices.getUserMedia({[this.kind]:!0}).then(t=>{this.permission.set(!0);const s=t.getTracks().at(0)?.getSettings().deviceId;s&&this.preferred.set(s),t.getTracks().forEach(r=>{r.stop()})}).catch(()=>{})}close(){this.signals.close()}}class uo{enabled;device;constraints;source=new h(void 0);signals=new S;constructor(t){this.device=new Xt("video",t?.device),this.enabled=h.from(t?.enabled??!1),this.constraints=h.from(t?.constraints),this.signals.run(this.#e.bind(this))}#e(t){if(!t.get(this.enabled))return;const s=t.get(this.device.requested),r={...t.get(this.constraints)??{},deviceId:s?{exact:s}:void 0};t.spawn(async()=>{const i=navigator.mediaDevices.getUserMedia({video:r}).catch(()=>{});t.cleanup(()=>i.then(c=>c?.getTracks().forEach(u=>{u.stop()})));const n=await Promise.race([i,t.cancel]);if(!n)return;this.device.permission.set(!0);const a=n.getVideoTracks()[0];if(!a)return;const o=a.getSettings();t.set(this.device.active,o.deviceId),t.set(this.source,a)})}close(){this.signals.close(),this.device.close()}}let lo=class{file=new h(void 0);signals=new S;source=new h({});enabled;constructor(e){this.enabled=h.from(e.enabled??!1),this.file=h.from(e.file),this.signals.run(t=>{const s=t.getAll([this.file,this.enabled]);if(!s)return;const[r]=s;this.#e(r,t).catch(i=>{console.error("Failed to decode file:",i)})})}async#e(e,t){const s=e.type;if(s.startsWith("image/"))await this.#t(e,t);else if(s.startsWith("video/")||s.startsWith("audio/"))await this.#s(e,t);else throw new Error(`Unsupported file type: ${s}`)}async#t(e,t){const s=new Image,r=URL.createObjectURL(e);s.src=r,await s.decode(),t.cleanup(()=>URL.revokeObjectURL(r));const i=document.createElement("canvas");i.width=s.width,i.height=s.height;const n=i.getContext("2d");if(!n)throw new Error("Failed to create 2D canvas context");const a=setInterval(()=>{n.drawImage(s,0,0)},1e3/30);t.cleanup(()=>clearInterval(a));const o=i.captureStream(30).getVideoTracks()[0];if(!o)throw new Error("Failed to capture video track from canvas stream");t.set(this.source,{video:o},{})}async#s(e,t){const s=document.createElement("video"),r=URL.createObjectURL(e);s.src=r,s.loop=!0,s.muted=!0,await new Promise((o,c)=>{s.onloadedmetadata=()=>o(),s.onerror=()=>c(new Error("Failed to load video"))}),await s.play(),t.cleanup(()=>{s.pause(),URL.revokeObjectURL(r)});const i=s.captureStream(),n=i.getVideoTracks()[0],a=i.getAudioTracks()[0];if(!n&&!a)throw new Error("Failed to capture any tracks from video element");t.set(this.source,{video:n,audio:a},{})}close(){this.signals.close()}};class ho{enabled;device;constraints;source=new h(void 0);signals=new S;constructor(t){this.device=new Xt("audio",t?.device),this.enabled=h.from(t?.enabled??!1),this.constraints=h.from(t?.constraints),this.signals.run(this.#e.bind(this))}#e(t){if(!t.get(this.enabled))return;const s=t.get(this.device.requested),r={...t.get(this.constraints)??{},deviceId:s!==void 0?{exact:s}:void 0};t.spawn(async()=>{const i=navigator.mediaDevices.getUserMedia({audio:r}).catch(()=>{});t.cleanup(()=>i.then(c=>c?.getTracks().forEach(u=>{u.stop()})));const n=await Promise.race([i,t.cancel]);if(!n)return;this.device.permission.set(!0);const a=n.getAudioTracks()[0];if(!a)return;const o=a.getSettings();s===void 0&&this.device.preferred.set(o.deviceId),t.set(this.device.active,o.deviceId),t.set(this.source,a)})}close(){this.signals.close(),this.device.close()}}class fo{enabled;video;audio;source=new h(void 0);signals=new S;constructor(t){this.enabled=h.from(t?.enabled??!1),this.video=h.from(t?.video),this.audio=h.from(t?.audio),this.signals.run(this.#e.bind(this))}#e(t){if(!t.get(this.enabled))return;const s=t.get(this.video),r=t.get(this.audio);let i;typeof self.CaptureController<"u"&&(i=new CaptureController,i.setFocusBehavior("no-focus-change")),t.spawn(async()=>{const n=await Promise.race([navigator.mediaDevices.getDisplayMedia({video:s,audio:r,controller:i,preferCurrentTab:!1,selfBrowserSurface:"exclude",surfaceSwitching:"include"}).catch(()=>{}),t.cancel]);if(!n)return;const a=n.getVideoTracks().at(0),o=n.getAudioTracks().at(0);t.cleanup(()=>a?.stop()),t.cleanup(()=>o?.stop()),t.set(this.source,{video:a,audio:o})})}close(){this.signals.close()}}const po=["url","name","muted","invisible","source"],mo=new FinalizationRegistry(e=>e.close());class wo extends HTMLElement{static observedAttributes=po;state={source:new h(void 0),muted:new h(!1),invisible:new h(!1)};connection;broadcast;#e=new h(void 0);video=new h(void 0);audio=new h(void 0);file=new h(void 0);#t;#s;#r;#i=new h(!1);signals=new S;constructor(){super(),mo.register(this,this.signals),this.connection=new rs({enabled:this.#i}),this.signals.cleanup(()=>this.connection.close()),this.#t=new h(!1),this.#s=new h(!1),this.#r=new h(!1),this.signals.run(r=>{const i=r.get(this.state.muted),n=r.get(this.state.invisible);this.#t.set(!n),this.#s.set(!i),this.#r.set(!i||!n)}),this.broadcast=new Ce({connection:this.connection.established,enabled:this.#i,audio:{enabled:this.#s},video:{hd:{enabled:this.#t}}}),this.signals.cleanup(()=>this.broadcast.close());const t=()=>{this.#e.set(this.querySelector("video"))},s=new MutationObserver(t);s.observe(this,{childList:!0,subtree:!0}),this.signals.cleanup(()=>s.disconnect()),t(),this.signals.run(r=>{const i=r.get(this.#e);if(!i)return;const n=r.get(this.broadcast.video.source);if(!n){i.style.display="none";return}i.srcObject=new MediaStream([n]),i.style.display="block",r.cleanup(()=>{i.srcObject=null})}),this.signals.run(this.#n.bind(this))}connectedCallback(){this.#i.set(!0)}disconnectedCallback(){this.#i.set(!1)}attributeChangedCallback(t,s,r){if(s!==r)if(t==="url")this.connection.url.set(r?new URL(r):void 0);else if(t==="name")this.broadcast.name.set(De(r??""));else if(t==="source")if(r==="camera"||r==="screen"||r==="file"||r===null)this.state.source.set(r);else throw new Error(`Invalid source: ${r}`);else if(t==="muted")this.state.muted.set(r!==null);else if(t==="invisible")this.state.invisible.set(r!==null);else{const i=t;throw new Error(`Invalid attribute: ${i}`)}}#n(t){const s=t.get(this.state.source);if(!s)return;if(s==="camera"){const i=new uo({enabled:this.#t});this.signals.run(a=>{const o=a.get(i.source);this.broadcast.video.source.set(o)});const n=new ho({enabled:this.#s});this.signals.run(a=>{const o=a.get(n.source);this.broadcast.audio.source.set(o)}),t.set(this.video,i),t.set(this.audio,n),t.cleanup(()=>{i.close(),n.close()});return}if(s==="screen"){const i=new fo({enabled:this.#r});this.signals.run(n=>{const a=n.get(i.source);a&&(n.set(this.broadcast.video.source,a.video),n.set(this.broadcast.audio.source,a.audio))}),t.set(this.video,i),t.set(this.audio,i),t.cleanup(()=>{i.close()});return}if(s==="file"||s instanceof File){const i=new lo({file:s instanceof File?s:void 0,enabled:this.#r});this.signals.run(n=>{const a=n.get(i.source);this.broadcast.video.source.set(a.video),this.broadcast.audio.source.set(a.audio)}),t.cleanup(()=>{i.close()});return}const r=s;throw new Error(`Invalid source: ${r}`)}get url(){return this.connection.url.peek()}set url(t){this.connection.url.set(t?new URL(t):void 0)}get name(){return this.broadcast.name.peek()}set name(t){this.broadcast.name.set(De(t))}get source(){return this.state.source.peek()}set source(t){this.state.source.set(t)}get muted(){return this.state.muted.peek()}set muted(t){this.state.muted.set(t)}get invisible(){return this.state.invisible.peek()}set invisible(t){this.state.invisible.set(t)}}customElements.define("moq-publish",wo);const Me=document.getElementById("publish");if(!Me)throw new Error("missing <moq-publish> element");const Ht=new URLSearchParams(window.location.search),vo=Ht.get("name")??"hello",bo=Ht.get("url")??`${window.location.origin}/`;Me.setAttribute("url",bo);Me.setAttribute("name",vo);
