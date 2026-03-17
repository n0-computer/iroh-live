import{S as p,M as w,E as R,e as As,a as Me,d as Os,_ as Je,R as Ps,f as We}from"./preload-helper-DcOveGHk.js";function l(e,t,s){function r(o,c){if(o._zod||Object.defineProperty(o,"_zod",{value:{def:c,constr:a,traits:new Set},enumerable:!1}),o._zod.traits.has(e))return;o._zod.traits.add(e),t(o,c);const u=a.prototype,d=Object.keys(u);for(let f=0;f<d.length;f++){const h=d[f];h in o||(o[h]=u[h].bind(o))}}const i=s?.Parent??Object;class n extends i{}Object.defineProperty(n,"name",{value:e});function a(o){var c;const u=s?.Parent?new n:this;r(u,o),(c=u._zod).deferred??(c.deferred=[]);for(const d of u._zod.deferred)d();return u}return Object.defineProperty(a,"init",{value:r}),Object.defineProperty(a,Symbol.hasInstance,{value:o=>s?.Parent&&o instanceof s.Parent?!0:o?._zod?.traits?.has(e)}),Object.defineProperty(a,"name",{value:e}),a}class re extends Error{constructor(){super("Encountered Promise during synchronous parse. Use .parseAsync() instead.")}}class Pt extends Error{constructor(t){super(`Encountered unidirectional transform during encode: ${t}`),this.name="ZodEncodeError"}}const Tt={};function G(e){return Tt}function Ut(e){const t=Object.values(e).filter(s=>typeof s=="number");return Object.entries(e).filter(([s,r])=>t.indexOf(+s)===-1).map(([s,r])=>r)}function Ne(e,t){return typeof t=="bigint"?t.toString():t}function Se(e){return{get value(){{const t=e();return Object.defineProperty(this,"value",{value:t}),t}}}}function $e(e){return e==null}function Ce(e){const t=e.startsWith("^")?1:0,s=e.endsWith("$")?e.length-1:e.length;return e.slice(t,s)}function Ts(e,t){const s=(e.toString().split(".")[1]||"").length,r=t.toString();let i=(r.split(".")[1]||"").length;if(i===0&&/\d?e-\d?/.test(r)){const c=r.match(/\d?e-(\d?)/);c?.[1]&&(i=Number.parseInt(c[1]))}const n=s>i?s:i,a=Number.parseInt(e.toFixed(n).replace(".","")),o=Number.parseInt(t.toFixed(n).replace(".",""));return a%o/10**n}const He=Symbol("evaluating");function g(e,t,s){let r;Object.defineProperty(e,t,{get(){if(r!==He)return r===void 0&&(r=He,r=s()),r},set(i){Object.defineProperty(e,t,{value:i})},configurable:!0})}function Q(e,t,s){Object.defineProperty(e,t,{value:s,writable:!0,enumerable:!0,configurable:!0})}function H(...e){const t={};for(const s of e){const r=Object.getOwnPropertyDescriptors(s);Object.assign(t,r)}return Object.defineProperties({},t)}function Xe(e){return JSON.stringify(e)}function Us(e){return e.toLowerCase().trim().replace(/[^\w\s-]/g,"").replace(/[\s_-]+/g,"-").replace(/^-+|-+$/g,"")}const Nt="captureStackTrace"in Error?Error.captureStackTrace:(...e)=>{};function he(e){return typeof e=="object"&&e!==null&&!Array.isArray(e)}const Ns=Se(()=>{if(typeof navigator<"u"&&navigator?.userAgent?.includes("Cloudflare"))return!1;try{const e=Function;return new e(""),!0}catch{return!1}});function ie(e){if(he(e)===!1)return!1;const t=e.constructor;if(t===void 0||typeof t!="function")return!0;const s=t.prototype;return!(he(s)===!1||Object.prototype.hasOwnProperty.call(s,"isPrototypeOf")===!1)}function Rt(e){return ie(e)?{...e}:Array.isArray(e)?[...e]:e}const Rs=new Set(["string","number","symbol"]);function ne(e){return e.replace(/[.*+?^${}()|[\]\\]/g,"\\$&")}function X(e,t,s){const r=new e._zod.constr(t??e._zod.def);return(!t||s?.parent)&&(r._zod.parent=e),r}function m(e){const t=e;if(!t)return{};if(typeof t=="string")return{error:()=>t};if(t?.message!==void 0){if(t?.error!==void 0)throw new Error("Cannot specify both `message` and `error` params");t.error=t.message}return delete t.message,typeof t.error=="string"?{...t,error:()=>t.error}:t}function qs(e){return Object.keys(e).filter(t=>e[t]._zod.optin==="optional"&&e[t]._zod.optout==="optional")}const Ms={safeint:[Number.MIN_SAFE_INTEGER,Number.MAX_SAFE_INTEGER],int32:[-2147483648,2147483647],uint32:[0,4294967295],float32:[-34028234663852886e22,34028234663852886e22],float64:[-Number.MAX_VALUE,Number.MAX_VALUE]};function $s(e,t){const s=e._zod.def,r=s.checks;if(r&&r.length>0)throw new Error(".pick() cannot be used on object schemas containing refinements");const i=H(e._zod.def,{get shape(){const n={};for(const a in t){if(!(a in s.shape))throw new Error(`Unrecognized key: "${a}"`);t[a]&&(n[a]=s.shape[a])}return Q(this,"shape",n),n},checks:[]});return X(e,i)}function Cs(e,t){const s=e._zod.def,r=s.checks;if(r&&r.length>0)throw new Error(".omit() cannot be used on object schemas containing refinements");const i=H(e._zod.def,{get shape(){const n={...e._zod.def.shape};for(const a in t){if(!(a in s.shape))throw new Error(`Unrecognized key: "${a}"`);t[a]&&delete n[a]}return Q(this,"shape",n),n},checks:[]});return X(e,i)}function Ds(e,t){if(!ie(t))throw new Error("Invalid input to extend: expected a plain object");const s=e._zod.def.checks;if(s&&s.length>0){const i=e._zod.def.shape;for(const n in t)if(Object.getOwnPropertyDescriptor(i,n)!==void 0)throw new Error("Cannot overwrite keys on object schemas containing refinements. Use `.safeExtend()` instead.")}const r=H(e._zod.def,{get shape(){const i={...e._zod.def.shape,...t};return Q(this,"shape",i),i}});return X(e,r)}function Fs(e,t){if(!ie(t))throw new Error("Invalid input to safeExtend: expected a plain object");const s=H(e._zod.def,{get shape(){const r={...e._zod.def.shape,...t};return Q(this,"shape",r),r}});return X(e,s)}function Vs(e,t){const s=H(e._zod.def,{get shape(){const r={...e._zod.def.shape,...t._zod.def.shape};return Q(this,"shape",r),r},get catchall(){return t._zod.def.catchall},checks:[]});return X(e,s)}function js(e,t,s){const r=t._zod.def.checks;if(r&&r.length>0)throw new Error(".partial() cannot be used on object schemas containing refinements");const i=H(t._zod.def,{get shape(){const n=t._zod.def.shape,a={...n};if(s)for(const o in s){if(!(o in n))throw new Error(`Unrecognized key: "${o}"`);s[o]&&(a[o]=e?new e({type:"optional",innerType:n[o]}):n[o])}else for(const o in n)a[o]=e?new e({type:"optional",innerType:n[o]}):n[o];return Q(this,"shape",a),a},checks:[]});return X(t,i)}function Ls(e,t,s){const r=H(t._zod.def,{get shape(){const i=t._zod.def.shape,n={...i};if(s)for(const a in s){if(!(a in n))throw new Error(`Unrecognized key: "${a}"`);s[a]&&(n[a]=new e({type:"nonoptional",innerType:i[a]}))}else for(const a in i)n[a]=new e({type:"nonoptional",innerType:i[a]});return Q(this,"shape",n),n}});return X(t,r)}function ee(e,t=0){if(e.aborted===!0)return!0;for(let s=t;s<e.issues.length;s++)if(e.issues[s]?.continue!==!0)return!0;return!1}function te(e,t){return t.map(s=>{var r;return(r=s).path??(r.path=[]),s.path.unshift(e),s})}function be(e){return typeof e=="string"?e:e?.message}function J(e,t,s){const r={...e,path:e.path??[]};if(!e.message){const i=be(e.inst?._zod.def?.error?.(e))??be(t?.error?.(e))??be(s.customError?.(e))??be(s.localeError?.(e))??"Invalid input";r.message=i}return delete r.inst,delete r.continue,t?.reportInput||delete r.input,r}function De(e){return Array.isArray(e)?"array":typeof e=="string"?"string":"unknown"}function pe(...e){const[t,s,r]=e;return typeof t=="string"?{message:t,code:"custom",input:s,inst:r}:{...t}}const qt=(e,t)=>{e.name="$ZodError",Object.defineProperty(e,"_zod",{value:e._zod,enumerable:!1}),Object.defineProperty(e,"issues",{value:t,enumerable:!1}),e.message=JSON.stringify(t,Ne,2),Object.defineProperty(e,"toString",{value:()=>e.message,enumerable:!1})},Mt=l("$ZodError",qt),$t=l("$ZodError",qt,{Parent:Error});function Zs(e,t=s=>s.message){const s={},r=[];for(const i of e.issues)i.path.length>0?(s[i.path[0]]=s[i.path[0]]||[],s[i.path[0]].push(t(i))):r.push(t(i));return{formErrors:r,fieldErrors:s}}function Bs(e,t=s=>s.message){const s={_errors:[]},r=i=>{for(const n of i.issues)if(n.code==="invalid_union"&&n.errors.length)n.errors.map(a=>r({issues:a}));else if(n.code==="invalid_key")r({issues:n.issues});else if(n.code==="invalid_element")r({issues:n.issues});else if(n.path.length===0)s._errors.push(t(n));else{let a=s,o=0;for(;o<n.path.length;){const c=n.path[o];o===n.path.length-1?(a[c]=a[c]||{_errors:[]},a[c]._errors.push(t(n))):a[c]=a[c]||{_errors:[]},a=a[c],o++}}};return r(e),s}const Fe=e=>(t,s,r,i)=>{const n=r?Object.assign(r,{async:!1}):{async:!1},a=t._zod.run({value:s,issues:[]},n);if(a instanceof Promise)throw new re;if(a.issues.length){const o=new(i?.Err??e)(a.issues.map(c=>J(c,n,G())));throw Nt(o,i?.callee),o}return a.value},Ve=e=>async(t,s,r,i)=>{const n=r?Object.assign(r,{async:!0}):{async:!0};let a=t._zod.run({value:s,issues:[]},n);if(a instanceof Promise&&(a=await a),a.issues.length){const o=new(i?.Err??e)(a.issues.map(c=>J(c,n,G())));throw Nt(o,i?.callee),o}return a.value},ze=e=>(t,s,r)=>{const i=r?{...r,async:!1}:{async:!1},n=t._zod.run({value:s,issues:[]},i);if(n instanceof Promise)throw new re;return n.issues.length?{success:!1,error:new(e??Mt)(n.issues.map(a=>J(a,i,G())))}:{success:!0,data:n.value}},Gs=ze($t),Ee=e=>async(t,s,r)=>{const i=r?Object.assign(r,{async:!0}):{async:!0};let n=t._zod.run({value:s,issues:[]},i);return n instanceof Promise&&(n=await n),n.issues.length?{success:!1,error:new e(n.issues.map(a=>J(a,i,G())))}:{success:!0,data:n.value}},Js=Ee($t),Ws=e=>(t,s,r)=>{const i=r?Object.assign(r,{direction:"backward"}):{direction:"backward"};return Fe(e)(t,s,i)},Hs=e=>(t,s,r)=>Fe(e)(t,s,r),Xs=e=>async(t,s,r)=>{const i=r?Object.assign(r,{direction:"backward"}):{direction:"backward"};return Ve(e)(t,s,i)},Ys=e=>async(t,s,r)=>Ve(e)(t,s,r),Ks=e=>(t,s,r)=>{const i=r?Object.assign(r,{direction:"backward"}):{direction:"backward"};return ze(e)(t,s,i)},Qs=e=>(t,s,r)=>ze(e)(t,s,r),er=e=>async(t,s,r)=>{const i=r?Object.assign(r,{direction:"backward"}):{direction:"backward"};return Ee(e)(t,s,i)},tr=e=>async(t,s,r)=>Ee(e)(t,s,r),sr=/^[cC][^\s-]{8,}$/,rr=/^[0-9a-z]+$/,ir=/^[0-9A-HJKMNP-TV-Za-hjkmnp-tv-z]{26}$/,nr=/^[0-9a-vA-V]{20}$/,ar=/^[A-Za-z0-9]{27}$/,or=/^[a-zA-Z0-9_-]{21}$/,cr=/^P(?:(\d+W)|(?!.*W)(?=\d|T\d)(\d+Y)?(\d+M)?(\d+D)?(T(?=\d)(\d+H)?(\d+M)?(\d+([.,]\d+)?S)?)?)$/,ur=/^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$/,Ye=e=>e?new RegExp(`^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-${e}[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12})$`):/^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-8][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}|00000000-0000-0000-0000-000000000000|ffffffff-ffff-ffff-ffff-ffffffffffff)$/,dr=/^(?!\.)(?!.*\.\.)([A-Za-z0-9_'+\-\.]*)[A-Za-z0-9_+-]@([A-Za-z0-9][A-Za-z0-9\-]*\.)+[A-Za-z]{2,}$/,lr="^(\\p{Extended_Pictographic}|\\p{Emoji_Component})+$";function fr(){return new RegExp(lr,"u")}const hr=/^(?:(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])$/,pr=/^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,7}:|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})|:((:[0-9a-fA-F]{1,4}){1,7}|:))$/,mr=/^((25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\.){3}(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\/([0-9]|[1-2][0-9]|3[0-2])$/,wr=/^(([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|::|([0-9a-fA-F]{1,4})?::([0-9a-fA-F]{1,4}:?){0,6})\/(12[0-8]|1[01][0-9]|[1-9]?[0-9])$/,br=/^$|^(?:[0-9a-zA-Z+/]{4})*(?:(?:[0-9a-zA-Z+/]{2}==)|(?:[0-9a-zA-Z+/]{3}=))?$/,Ct=/^[A-Za-z0-9_-]*$/,vr=/^\+[1-9]\d{6,14}$/,Dt="(?:(?:\\d\\d[2468][048]|\\d\\d[13579][26]|\\d\\d0[48]|[02468][048]00|[13579][26]00)-02-29|\\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\\d|30)|(?:02)-(?:0[1-9]|1\\d|2[0-8])))",gr=new RegExp(`^${Dt}$`);function Ft(e){const t="(?:[01]\\d|2[0-3]):[0-5]\\d";return typeof e.precision=="number"?e.precision===-1?`${t}`:e.precision===0?`${t}:[0-5]\\d`:`${t}:[0-5]\\d\\.\\d{${e.precision}}`:`${t}(?::[0-5]\\d(?:\\.\\d+)?)?`}function yr(e){return new RegExp(`^${Ft(e)}$`)}function _r(e){const t=Ft({precision:e.precision}),s=["Z"];e.local&&s.push(""),e.offset&&s.push("([+-](?:[01]\\d|2[0-3]):[0-5]\\d)");const r=`${t}(?:${s.join("|")})`;return new RegExp(`^${Dt}T(?:${r})$`)}const kr=e=>{const t=e?`[\\s\\S]{${e?.minimum??0},${e?.maximum??""}}`:"[\\s\\S]*";return new RegExp(`^${t}$`)},Ir=/^-?\d+$/,Vt=/^-?\d+(?:\.\d+)?$/,Sr=/^(?:true|false)$/i,zr=/^[^A-Z]*$/,Er=/^[^a-z]*$/,$=l("$ZodCheck",(e,t)=>{var s;e._zod??(e._zod={}),e._zod.def=t,(s=e._zod).onattach??(s.onattach=[])}),jt={number:"number",bigint:"bigint",object:"date"},Lt=l("$ZodCheckLessThan",(e,t)=>{$.init(e,t);const s=jt[typeof t.value];e._zod.onattach.push(r=>{const i=r._zod.bag,n=(t.inclusive?i.maximum:i.exclusiveMaximum)??Number.POSITIVE_INFINITY;t.value<n&&(t.inclusive?i.maximum=t.value:i.exclusiveMaximum=t.value)}),e._zod.check=r=>{(t.inclusive?r.value<=t.value:r.value<t.value)||r.issues.push({origin:s,code:"too_big",maximum:typeof t.value=="object"?t.value.getTime():t.value,input:r.value,inclusive:t.inclusive,inst:e,continue:!t.abort})}}),Zt=l("$ZodCheckGreaterThan",(e,t)=>{$.init(e,t);const s=jt[typeof t.value];e._zod.onattach.push(r=>{const i=r._zod.bag,n=(t.inclusive?i.minimum:i.exclusiveMinimum)??Number.NEGATIVE_INFINITY;t.value>n&&(t.inclusive?i.minimum=t.value:i.exclusiveMinimum=t.value)}),e._zod.check=r=>{(t.inclusive?r.value>=t.value:r.value>t.value)||r.issues.push({origin:s,code:"too_small",minimum:typeof t.value=="object"?t.value.getTime():t.value,input:r.value,inclusive:t.inclusive,inst:e,continue:!t.abort})}}),xr=l("$ZodCheckMultipleOf",(e,t)=>{$.init(e,t),e._zod.onattach.push(s=>{var r;(r=s._zod.bag).multipleOf??(r.multipleOf=t.value)}),e._zod.check=s=>{if(typeof s.value!=typeof t.value)throw new Error("Cannot mix number and bigint in multiple_of check.");(typeof s.value=="bigint"?s.value%t.value===BigInt(0):Ts(s.value,t.value)===0)||s.issues.push({origin:typeof s.value,code:"not_multiple_of",divisor:t.value,input:s.value,inst:e,continue:!t.abort})}}),Ar=l("$ZodCheckNumberFormat",(e,t)=>{$.init(e,t),t.format=t.format||"float64";const s=t.format?.includes("int"),r=s?"int":"number",[i,n]=Ms[t.format];e._zod.onattach.push(a=>{const o=a._zod.bag;o.format=t.format,o.minimum=i,o.maximum=n,s&&(o.pattern=Ir)}),e._zod.check=a=>{const o=a.value;if(s){if(!Number.isInteger(o)){a.issues.push({expected:r,format:t.format,code:"invalid_type",continue:!1,input:o,inst:e});return}if(!Number.isSafeInteger(o)){o>0?a.issues.push({input:o,code:"too_big",maximum:Number.MAX_SAFE_INTEGER,note:"Integers must be within the safe integer range.",inst:e,origin:r,inclusive:!0,continue:!t.abort}):a.issues.push({input:o,code:"too_small",minimum:Number.MIN_SAFE_INTEGER,note:"Integers must be within the safe integer range.",inst:e,origin:r,inclusive:!0,continue:!t.abort});return}}o<i&&a.issues.push({origin:"number",input:o,code:"too_small",minimum:i,inclusive:!0,inst:e,continue:!t.abort}),o>n&&a.issues.push({origin:"number",input:o,code:"too_big",maximum:n,inclusive:!0,inst:e,continue:!t.abort})}}),Or=l("$ZodCheckMaxLength",(e,t)=>{var s;$.init(e,t),(s=e._zod.def).when??(s.when=r=>{const i=r.value;return!$e(i)&&i.length!==void 0}),e._zod.onattach.push(r=>{const i=r._zod.bag.maximum??Number.POSITIVE_INFINITY;t.maximum<i&&(r._zod.bag.maximum=t.maximum)}),e._zod.check=r=>{const i=r.value;if(i.length<=t.maximum)return;const n=De(i);r.issues.push({origin:n,code:"too_big",maximum:t.maximum,inclusive:!0,input:i,inst:e,continue:!t.abort})}}),Pr=l("$ZodCheckMinLength",(e,t)=>{var s;$.init(e,t),(s=e._zod.def).when??(s.when=r=>{const i=r.value;return!$e(i)&&i.length!==void 0}),e._zod.onattach.push(r=>{const i=r._zod.bag.minimum??Number.NEGATIVE_INFINITY;t.minimum>i&&(r._zod.bag.minimum=t.minimum)}),e._zod.check=r=>{const i=r.value;if(i.length>=t.minimum)return;const n=De(i);r.issues.push({origin:n,code:"too_small",minimum:t.minimum,inclusive:!0,input:i,inst:e,continue:!t.abort})}}),Tr=l("$ZodCheckLengthEquals",(e,t)=>{var s;$.init(e,t),(s=e._zod.def).when??(s.when=r=>{const i=r.value;return!$e(i)&&i.length!==void 0}),e._zod.onattach.push(r=>{const i=r._zod.bag;i.minimum=t.length,i.maximum=t.length,i.length=t.length}),e._zod.check=r=>{const i=r.value,n=i.length;if(n===t.length)return;const a=De(i),o=n>t.length;r.issues.push({origin:a,...o?{code:"too_big",maximum:t.length}:{code:"too_small",minimum:t.length},inclusive:!0,exact:!0,input:r.value,inst:e,continue:!t.abort})}}),xe=l("$ZodCheckStringFormat",(e,t)=>{var s,r;$.init(e,t),e._zod.onattach.push(i=>{const n=i._zod.bag;n.format=t.format,t.pattern&&(n.patterns??(n.patterns=new Set),n.patterns.add(t.pattern))}),t.pattern?(s=e._zod).check??(s.check=i=>{t.pattern.lastIndex=0,!t.pattern.test(i.value)&&i.issues.push({origin:"string",code:"invalid_format",format:t.format,input:i.value,...t.pattern?{pattern:t.pattern.toString()}:{},inst:e,continue:!t.abort})}):(r=e._zod).check??(r.check=()=>{})}),Ur=l("$ZodCheckRegex",(e,t)=>{xe.init(e,t),e._zod.check=s=>{t.pattern.lastIndex=0,!t.pattern.test(s.value)&&s.issues.push({origin:"string",code:"invalid_format",format:"regex",input:s.value,pattern:t.pattern.toString(),inst:e,continue:!t.abort})}}),Nr=l("$ZodCheckLowerCase",(e,t)=>{t.pattern??(t.pattern=zr),xe.init(e,t)}),Rr=l("$ZodCheckUpperCase",(e,t)=>{t.pattern??(t.pattern=Er),xe.init(e,t)}),qr=l("$ZodCheckIncludes",(e,t)=>{$.init(e,t);const s=ne(t.includes),r=new RegExp(typeof t.position=="number"?`^.{${t.position}}${s}`:s);t.pattern=r,e._zod.onattach.push(i=>{const n=i._zod.bag;n.patterns??(n.patterns=new Set),n.patterns.add(r)}),e._zod.check=i=>{i.value.includes(t.includes,t.position)||i.issues.push({origin:"string",code:"invalid_format",format:"includes",includes:t.includes,input:i.value,inst:e,continue:!t.abort})}}),Mr=l("$ZodCheckStartsWith",(e,t)=>{$.init(e,t);const s=new RegExp(`^${ne(t.prefix)}.*`);t.pattern??(t.pattern=s),e._zod.onattach.push(r=>{const i=r._zod.bag;i.patterns??(i.patterns=new Set),i.patterns.add(s)}),e._zod.check=r=>{r.value.startsWith(t.prefix)||r.issues.push({origin:"string",code:"invalid_format",format:"starts_with",prefix:t.prefix,input:r.value,inst:e,continue:!t.abort})}}),$r=l("$ZodCheckEndsWith",(e,t)=>{$.init(e,t);const s=new RegExp(`.*${ne(t.suffix)}$`);t.pattern??(t.pattern=s),e._zod.onattach.push(r=>{const i=r._zod.bag;i.patterns??(i.patterns=new Set),i.patterns.add(s)}),e._zod.check=r=>{r.value.endsWith(t.suffix)||r.issues.push({origin:"string",code:"invalid_format",format:"ends_with",suffix:t.suffix,input:r.value,inst:e,continue:!t.abort})}}),Cr=l("$ZodCheckOverwrite",(e,t)=>{$.init(e,t),e._zod.check=s=>{s.value=t.tx(s.value)}});class Dr{constructor(t=[]){this.content=[],this.indent=0,this&&(this.args=t)}indented(t){this.indent+=1,t(this),this.indent-=1}write(t){if(typeof t=="function"){t(this,{execution:"sync"}),t(this,{execution:"async"});return}const s=t.split(`
`).filter(n=>n),r=Math.min(...s.map(n=>n.length-n.trimStart().length)),i=s.map(n=>n.slice(r)).map(n=>" ".repeat(this.indent*2)+n);for(const n of i)this.content.push(n)}compile(){const t=Function,s=this?.args,r=[...(this?.content??[""]).map(i=>`  ${i}`)];return new t(...s,r.join(`
`))}}const Fr={major:4,minor:3,patch:6},S=l("$ZodType",(e,t)=>{var s;e??(e={}),e._zod.def=t,e._zod.bag=e._zod.bag||{},e._zod.version=Fr;const r=[...e._zod.def.checks??[]];e._zod.traits.has("$ZodCheck")&&r.unshift(e);for(const i of r)for(const n of i._zod.onattach)n(e);if(r.length===0)(s=e._zod).deferred??(s.deferred=[]),e._zod.deferred?.push(()=>{e._zod.run=e._zod.parse});else{const i=(a,o,c)=>{let u=ee(a),d;for(const f of o){if(f._zod.def.when){if(!f._zod.def.when(a))continue}else if(u)continue;const h=a.issues.length,b=f._zod.check(a);if(b instanceof Promise&&c?.async===!1)throw new re;if(d||b instanceof Promise)d=(d??Promise.resolve()).then(async()=>{await b,a.issues.length!==h&&(u||(u=ee(a,h)))});else{if(a.issues.length===h)continue;u||(u=ee(a,h))}}return d?d.then(()=>a):a},n=(a,o,c)=>{if(ee(a))return a.aborted=!0,a;const u=i(o,r,c);if(u instanceof Promise){if(c.async===!1)throw new re;return u.then(d=>e._zod.parse(d,c))}return e._zod.parse(u,c)};e._zod.run=(a,o)=>{if(o.skipChecks)return e._zod.parse(a,o);if(o.direction==="backward"){const u=e._zod.parse({value:a.value,issues:[]},{...o,skipChecks:!0});return u instanceof Promise?u.then(d=>n(d,a,o)):n(u,a,o)}const c=e._zod.parse(a,o);if(c instanceof Promise){if(o.async===!1)throw new re;return c.then(u=>i(u,r,o))}return i(c,r,o)}}g(e,"~standard",()=>({validate:i=>{try{const n=Gs(e,i);return n.success?{value:n.data}:{issues:n.error?.issues}}catch{return Js(e,i).then(n=>n.success?{value:n.data}:{issues:n.error?.issues})}},vendor:"zod",version:1}))}),je=l("$ZodString",(e,t)=>{S.init(e,t),e._zod.pattern=[...e?._zod.bag?.patterns??[]].pop()??kr(e._zod.bag),e._zod.parse=(s,r)=>{if(t.coerce)try{s.value=String(s.value)}catch{}return typeof s.value=="string"||s.issues.push({expected:"string",code:"invalid_type",input:s.value,inst:e}),s}}),_=l("$ZodStringFormat",(e,t)=>{xe.init(e,t),je.init(e,t)}),Vr=l("$ZodGUID",(e,t)=>{t.pattern??(t.pattern=ur),_.init(e,t)}),jr=l("$ZodUUID",(e,t)=>{if(t.version){const s={v1:1,v2:2,v3:3,v4:4,v5:5,v6:6,v7:7,v8:8}[t.version];if(s===void 0)throw new Error(`Invalid UUID version: "${t.version}"`);t.pattern??(t.pattern=Ye(s))}else t.pattern??(t.pattern=Ye());_.init(e,t)}),Lr=l("$ZodEmail",(e,t)=>{t.pattern??(t.pattern=dr),_.init(e,t)}),Zr=l("$ZodURL",(e,t)=>{_.init(e,t),e._zod.check=s=>{try{const r=s.value.trim(),i=new URL(r);t.hostname&&(t.hostname.lastIndex=0,t.hostname.test(i.hostname)||s.issues.push({code:"invalid_format",format:"url",note:"Invalid hostname",pattern:t.hostname.source,input:s.value,inst:e,continue:!t.abort})),t.protocol&&(t.protocol.lastIndex=0,t.protocol.test(i.protocol.endsWith(":")?i.protocol.slice(0,-1):i.protocol)||s.issues.push({code:"invalid_format",format:"url",note:"Invalid protocol",pattern:t.protocol.source,input:s.value,inst:e,continue:!t.abort})),t.normalize?s.value=i.href:s.value=r;return}catch{s.issues.push({code:"invalid_format",format:"url",input:s.value,inst:e,continue:!t.abort})}}}),Br=l("$ZodEmoji",(e,t)=>{t.pattern??(t.pattern=fr()),_.init(e,t)}),Gr=l("$ZodNanoID",(e,t)=>{t.pattern??(t.pattern=or),_.init(e,t)}),Jr=l("$ZodCUID",(e,t)=>{t.pattern??(t.pattern=sr),_.init(e,t)}),Wr=l("$ZodCUID2",(e,t)=>{t.pattern??(t.pattern=rr),_.init(e,t)}),Hr=l("$ZodULID",(e,t)=>{t.pattern??(t.pattern=ir),_.init(e,t)}),Xr=l("$ZodXID",(e,t)=>{t.pattern??(t.pattern=nr),_.init(e,t)}),Yr=l("$ZodKSUID",(e,t)=>{t.pattern??(t.pattern=ar),_.init(e,t)}),Kr=l("$ZodISODateTime",(e,t)=>{t.pattern??(t.pattern=_r(t)),_.init(e,t)}),Qr=l("$ZodISODate",(e,t)=>{t.pattern??(t.pattern=gr),_.init(e,t)}),ei=l("$ZodISOTime",(e,t)=>{t.pattern??(t.pattern=yr(t)),_.init(e,t)}),ti=l("$ZodISODuration",(e,t)=>{t.pattern??(t.pattern=cr),_.init(e,t)}),si=l("$ZodIPv4",(e,t)=>{t.pattern??(t.pattern=hr),_.init(e,t),e._zod.bag.format="ipv4"}),ri=l("$ZodIPv6",(e,t)=>{t.pattern??(t.pattern=pr),_.init(e,t),e._zod.bag.format="ipv6",e._zod.check=s=>{try{new URL(`http://[${s.value}]`)}catch{s.issues.push({code:"invalid_format",format:"ipv6",input:s.value,inst:e,continue:!t.abort})}}}),ii=l("$ZodCIDRv4",(e,t)=>{t.pattern??(t.pattern=mr),_.init(e,t)}),ni=l("$ZodCIDRv6",(e,t)=>{t.pattern??(t.pattern=wr),_.init(e,t),e._zod.check=s=>{const r=s.value.split("/");try{if(r.length!==2)throw new Error;const[i,n]=r;if(!n)throw new Error;const a=Number(n);if(`${a}`!==n)throw new Error;if(a<0||a>128)throw new Error;new URL(`http://[${i}]`)}catch{s.issues.push({code:"invalid_format",format:"cidrv6",input:s.value,inst:e,continue:!t.abort})}}});function Bt(e){if(e==="")return!0;if(e.length%4!==0)return!1;try{return atob(e),!0}catch{return!1}}const ai=l("$ZodBase64",(e,t)=>{t.pattern??(t.pattern=br),_.init(e,t),e._zod.bag.contentEncoding="base64",e._zod.check=s=>{Bt(s.value)||s.issues.push({code:"invalid_format",format:"base64",input:s.value,inst:e,continue:!t.abort})}});function oi(e){if(!Ct.test(e))return!1;const t=e.replace(/[-_]/g,r=>r==="-"?"+":"/"),s=t.padEnd(Math.ceil(t.length/4)*4,"=");return Bt(s)}const ci=l("$ZodBase64URL",(e,t)=>{t.pattern??(t.pattern=Ct),_.init(e,t),e._zod.bag.contentEncoding="base64url",e._zod.check=s=>{oi(s.value)||s.issues.push({code:"invalid_format",format:"base64url",input:s.value,inst:e,continue:!t.abort})}}),ui=l("$ZodE164",(e,t)=>{t.pattern??(t.pattern=vr),_.init(e,t)});function di(e,t=null){try{const s=e.split(".");if(s.length!==3)return!1;const[r]=s;if(!r)return!1;const i=JSON.parse(atob(r));return!("typ"in i&&i?.typ!=="JWT"||!i.alg||t&&(!("alg"in i)||i.alg!==t))}catch{return!1}}const li=l("$ZodJWT",(e,t)=>{_.init(e,t),e._zod.check=s=>{di(s.value,t.alg)||s.issues.push({code:"invalid_format",format:"jwt",input:s.value,inst:e,continue:!t.abort})}}),Gt=l("$ZodNumber",(e,t)=>{S.init(e,t),e._zod.pattern=e._zod.bag.pattern??Vt,e._zod.parse=(s,r)=>{if(t.coerce)try{s.value=Number(s.value)}catch{}const i=s.value;if(typeof i=="number"&&!Number.isNaN(i)&&Number.isFinite(i))return s;const n=typeof i=="number"?Number.isNaN(i)?"NaN":Number.isFinite(i)?void 0:"Infinity":void 0;return s.issues.push({expected:"number",code:"invalid_type",input:i,inst:e,...n?{received:n}:{}}),s}}),fi=l("$ZodNumberFormat",(e,t)=>{Ar.init(e,t),Gt.init(e,t)}),hi=l("$ZodBoolean",(e,t)=>{S.init(e,t),e._zod.pattern=Sr,e._zod.parse=(s,r)=>{if(t.coerce)try{s.value=!!s.value}catch{}const i=s.value;return typeof i=="boolean"||s.issues.push({expected:"boolean",code:"invalid_type",input:i,inst:e}),s}}),pi=l("$ZodUnknown",(e,t)=>{S.init(e,t),e._zod.parse=s=>s}),mi=l("$ZodNever",(e,t)=>{S.init(e,t),e._zod.parse=(s,r)=>(s.issues.push({expected:"never",code:"invalid_type",input:s.value,inst:e}),s)});function Ke(e,t,s){e.issues.length&&t.issues.push(...te(s,e.issues)),t.value[s]=e.value}const wi=l("$ZodArray",(e,t)=>{S.init(e,t),e._zod.parse=(s,r)=>{const i=s.value;if(!Array.isArray(i))return s.issues.push({expected:"array",code:"invalid_type",input:i,inst:e}),s;s.value=Array(i.length);const n=[];for(let a=0;a<i.length;a++){const o=i[a],c=t.element._zod.run({value:o,issues:[]},r);c instanceof Promise?n.push(c.then(u=>Ke(u,s,a))):Ke(c,s,a)}return n.length?Promise.all(n).then(()=>s):s}});function ye(e,t,s,r,i){if(e.issues.length){if(i&&!(s in r))return;t.issues.push(...te(s,e.issues))}e.value===void 0?s in r&&(t.value[s]=void 0):t.value[s]=e.value}function Jt(e){const t=Object.keys(e.shape);for(const r of t)if(!e.shape?.[r]?._zod?.traits?.has("$ZodType"))throw new Error(`Invalid element at key "${r}": expected a Zod schema`);const s=qs(e.shape);return{...e,keys:t,keySet:new Set(t),numKeys:t.length,optionalKeys:new Set(s)}}function Wt(e,t,s,r,i,n){const a=[],o=i.keySet,c=i.catchall._zod,u=c.def.type,d=c.optout==="optional";for(const f in t){if(o.has(f))continue;if(u==="never"){a.push(f);continue}const h=c.run({value:t[f],issues:[]},r);h instanceof Promise?e.push(h.then(b=>ye(b,s,f,t,d))):ye(h,s,f,t,d)}return a.length&&s.issues.push({code:"unrecognized_keys",keys:a,input:t,inst:n}),e.length?Promise.all(e).then(()=>s):s}const bi=l("$ZodObject",(e,t)=>{if(S.init(e,t),!Object.getOwnPropertyDescriptor(t,"shape")?.get){const a=t.shape;Object.defineProperty(t,"shape",{get:()=>{const o={...a};return Object.defineProperty(t,"shape",{value:o}),o}})}const s=Se(()=>Jt(t));g(e._zod,"propValues",()=>{const a=t.shape,o={};for(const c in a){const u=a[c]._zod;if(u.values){o[c]??(o[c]=new Set);for(const d of u.values)o[c].add(d)}}return o});const r=he,i=t.catchall;let n;e._zod.parse=(a,o)=>{n??(n=s.value);const c=a.value;if(!r(c))return a.issues.push({expected:"object",code:"invalid_type",input:c,inst:e}),a;a.value={};const u=[],d=n.shape;for(const f of n.keys){const h=d[f],b=h._zod.optout==="optional",v=h._zod.run({value:c[f],issues:[]},o);v instanceof Promise?u.push(v.then(U=>ye(U,a,f,c,b))):ye(v,a,f,c,b)}return i?Wt(u,c,a,o,s.value,e):u.length?Promise.all(u).then(()=>a):a}}),vi=l("$ZodObjectJIT",(e,t)=>{bi.init(e,t);const s=e._zod.parse,r=Se(()=>Jt(t)),i=f=>{const h=new Dr(["shape","payload","ctx"]),b=r.value,v=O=>{const y=Xe(O);return`shape[${y}]._zod.run({ value: input[${y}], issues: [] }, ctx)`};h.write("const input = payload.value;");const U=Object.create(null);let q=0;for(const O of b.keys)U[O]=`key_${q++}`;h.write("const newResult = {};");for(const O of b.keys){const y=U[O],x=Xe(O),C=f[O]?._zod?.optout==="optional";h.write(`const ${y} = ${v(O)};`),C?h.write(`
        if (${y}.issues.length) {
          if (${x} in input) {
            payload.issues = payload.issues.concat(${y}.issues.map(iss => ({
              ...iss,
              path: iss.path ? [${x}, ...iss.path] : [${x}]
            })));
          }
        }
        
        if (${y}.value === undefined) {
          if (${x} in input) {
            newResult[${x}] = undefined;
          }
        } else {
          newResult[${x}] = ${y}.value;
        }
        
      `):h.write(`
        if (${y}.issues.length) {
          payload.issues = payload.issues.concat(${y}.issues.map(iss => ({
            ...iss,
            path: iss.path ? [${x}, ...iss.path] : [${x}]
          })));
        }
        
        if (${y}.value === undefined) {
          if (${x} in input) {
            newResult[${x}] = undefined;
          }
        } else {
          newResult[${x}] = ${y}.value;
        }
        
      `)}h.write("payload.value = newResult;"),h.write("return payload;");const D=h.compile();return(O,y)=>D(f,O,y)};let n;const a=he,o=!Tt.jitless,c=o&&Ns.value,u=t.catchall;let d;e._zod.parse=(f,h)=>{d??(d=r.value);const b=f.value;return a(b)?o&&c&&h?.async===!1&&h.jitless!==!0?(n||(n=i(t.shape)),f=n(f,h),u?Wt([],b,f,h,d,e):f):s(f,h):(f.issues.push({expected:"object",code:"invalid_type",input:b,inst:e}),f)}});function Qe(e,t,s,r){for(const n of e)if(n.issues.length===0)return t.value=n.value,t;const i=e.filter(n=>!ee(n));return i.length===1?(t.value=i[0].value,i[0]):(t.issues.push({code:"invalid_union",input:t.value,inst:s,errors:e.map(n=>n.issues.map(a=>J(a,r,G())))}),t)}const Ht=l("$ZodUnion",(e,t)=>{S.init(e,t),g(e._zod,"optin",()=>t.options.some(i=>i._zod.optin==="optional")?"optional":void 0),g(e._zod,"optout",()=>t.options.some(i=>i._zod.optout==="optional")?"optional":void 0),g(e._zod,"values",()=>{if(t.options.every(i=>i._zod.values))return new Set(t.options.flatMap(i=>Array.from(i._zod.values)))}),g(e._zod,"pattern",()=>{if(t.options.every(i=>i._zod.pattern)){const i=t.options.map(n=>n._zod.pattern);return new RegExp(`^(${i.map(n=>Ce(n.source)).join("|")})$`)}});const s=t.options.length===1,r=t.options[0]._zod.run;e._zod.parse=(i,n)=>{if(s)return r(i,n);let a=!1;const o=[];for(const c of t.options){const u=c._zod.run({value:i.value,issues:[]},n);if(u instanceof Promise)o.push(u),a=!0;else{if(u.issues.length===0)return u;o.push(u)}}return a?Promise.all(o).then(c=>Qe(c,i,e,n)):Qe(o,i,e,n)}}),gi=l("$ZodDiscriminatedUnion",(e,t)=>{t.inclusive=!1,Ht.init(e,t);const s=e._zod.parse;g(e._zod,"propValues",()=>{const i={};for(const n of t.options){const a=n._zod.propValues;if(!a||Object.keys(a).length===0)throw new Error(`Invalid discriminated union option at index "${t.options.indexOf(n)}"`);for(const[o,c]of Object.entries(a)){i[o]||(i[o]=new Set);for(const u of c)i[o].add(u)}}return i});const r=Se(()=>{const i=t.options,n=new Map;for(const a of i){const o=a._zod.propValues?.[t.discriminator];if(!o||o.size===0)throw new Error(`Invalid discriminated union option at index "${t.options.indexOf(a)}"`);for(const c of o){if(n.has(c))throw new Error(`Duplicate discriminator value "${String(c)}"`);n.set(c,a)}}return n});e._zod.parse=(i,n)=>{const a=i.value;if(!he(a))return i.issues.push({code:"invalid_type",expected:"object",input:a,inst:e}),i;const o=r.value.get(a?.[t.discriminator]);return o?o._zod.run(i,n):t.unionFallback?s(i,n):(i.issues.push({code:"invalid_union",errors:[],note:"No matching discriminator",discriminator:t.discriminator,input:a,path:[t.discriminator],inst:e}),i)}}),yi=l("$ZodIntersection",(e,t)=>{S.init(e,t),e._zod.parse=(s,r)=>{const i=s.value,n=t.left._zod.run({value:i,issues:[]},r),a=t.right._zod.run({value:i,issues:[]},r);return n instanceof Promise||a instanceof Promise?Promise.all([n,a]).then(([o,c])=>et(s,o,c)):et(s,n,a)}});function Re(e,t){if(e===t)return{valid:!0,data:e};if(e instanceof Date&&t instanceof Date&&+e==+t)return{valid:!0,data:e};if(ie(e)&&ie(t)){const s=Object.keys(t),r=Object.keys(e).filter(n=>s.indexOf(n)!==-1),i={...e,...t};for(const n of r){const a=Re(e[n],t[n]);if(!a.valid)return{valid:!1,mergeErrorPath:[n,...a.mergeErrorPath]};i[n]=a.data}return{valid:!0,data:i}}if(Array.isArray(e)&&Array.isArray(t)){if(e.length!==t.length)return{valid:!1,mergeErrorPath:[]};const s=[];for(let r=0;r<e.length;r++){const i=e[r],n=t[r],a=Re(i,n);if(!a.valid)return{valid:!1,mergeErrorPath:[r,...a.mergeErrorPath]};s.push(a.data)}return{valid:!0,data:s}}return{valid:!1,mergeErrorPath:[]}}function et(e,t,s){const r=new Map;let i;for(const o of t.issues)if(o.code==="unrecognized_keys"){i??(i=o);for(const c of o.keys)r.has(c)||r.set(c,{}),r.get(c).l=!0}else e.issues.push(o);for(const o of s.issues)if(o.code==="unrecognized_keys")for(const c of o.keys)r.has(c)||r.set(c,{}),r.get(c).r=!0;else e.issues.push(o);const n=[...r].filter(([,o])=>o.l&&o.r).map(([o])=>o);if(n.length&&i&&e.issues.push({...i,keys:n}),ee(e))return e;const a=Re(t.value,s.value);if(!a.valid)throw new Error(`Unmergable intersection. Error path: ${JSON.stringify(a.mergeErrorPath)}`);return e.value=a.data,e}const _i=l("$ZodRecord",(e,t)=>{S.init(e,t),e._zod.parse=(s,r)=>{const i=s.value;if(!ie(i))return s.issues.push({expected:"record",code:"invalid_type",input:i,inst:e}),s;const n=[],a=t.keyType._zod.values;if(a){s.value={};const o=new Set;for(const u of a)if(typeof u=="string"||typeof u=="number"||typeof u=="symbol"){o.add(typeof u=="number"?u.toString():u);const d=t.valueType._zod.run({value:i[u],issues:[]},r);d instanceof Promise?n.push(d.then(f=>{f.issues.length&&s.issues.push(...te(u,f.issues)),s.value[u]=f.value})):(d.issues.length&&s.issues.push(...te(u,d.issues)),s.value[u]=d.value)}let c;for(const u in i)o.has(u)||(c=c??[],c.push(u));c&&c.length>0&&s.issues.push({code:"unrecognized_keys",input:i,inst:e,keys:c})}else{s.value={};for(const o of Reflect.ownKeys(i)){if(o==="__proto__")continue;let c=t.keyType._zod.run({value:o,issues:[]},r);if(c instanceof Promise)throw new Error("Async schemas not supported in object keys currently");if(typeof o=="string"&&Vt.test(o)&&c.issues.length){const d=t.keyType._zod.run({value:Number(o),issues:[]},r);if(d instanceof Promise)throw new Error("Async schemas not supported in object keys currently");d.issues.length===0&&(c=d)}if(c.issues.length){t.mode==="loose"?s.value[o]=i[o]:s.issues.push({code:"invalid_key",origin:"record",issues:c.issues.map(d=>J(d,r,G())),input:o,path:[o],inst:e});continue}const u=t.valueType._zod.run({value:i[o],issues:[]},r);u instanceof Promise?n.push(u.then(d=>{d.issues.length&&s.issues.push(...te(o,d.issues)),s.value[c.value]=d.value})):(u.issues.length&&s.issues.push(...te(o,u.issues)),s.value[c.value]=u.value)}}return n.length?Promise.all(n).then(()=>s):s}}),ki=l("$ZodEnum",(e,t)=>{S.init(e,t);const s=Ut(t.entries),r=new Set(s);e._zod.values=r,e._zod.pattern=new RegExp(`^(${s.filter(i=>Rs.has(typeof i)).map(i=>typeof i=="string"?ne(i):i.toString()).join("|")})$`),e._zod.parse=(i,n)=>{const a=i.value;return r.has(a)||i.issues.push({code:"invalid_value",values:s,input:a,inst:e}),i}}),Ii=l("$ZodLiteral",(e,t)=>{if(S.init(e,t),t.values.length===0)throw new Error("Cannot create literal schema with no valid values");const s=new Set(t.values);e._zod.values=s,e._zod.pattern=new RegExp(`^(${t.values.map(r=>typeof r=="string"?ne(r):r?ne(r.toString()):String(r)).join("|")})$`),e._zod.parse=(r,i)=>{const n=r.value;return s.has(n)||r.issues.push({code:"invalid_value",values:t.values,input:n,inst:e}),r}}),Si=l("$ZodTransform",(e,t)=>{S.init(e,t),e._zod.parse=(s,r)=>{if(r.direction==="backward")throw new Pt(e.constructor.name);const i=t.transform(s.value,s);if(r.async)return(i instanceof Promise?i:Promise.resolve(i)).then(n=>(s.value=n,s));if(i instanceof Promise)throw new re;return s.value=i,s}});function tt(e,t){return e.issues.length&&t===void 0?{issues:[],value:void 0}:e}const Xt=l("$ZodOptional",(e,t)=>{S.init(e,t),e._zod.optin="optional",e._zod.optout="optional",g(e._zod,"values",()=>t.innerType._zod.values?new Set([...t.innerType._zod.values,void 0]):void 0),g(e._zod,"pattern",()=>{const s=t.innerType._zod.pattern;return s?new RegExp(`^(${Ce(s.source)})?$`):void 0}),e._zod.parse=(s,r)=>{if(t.innerType._zod.optin==="optional"){const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(n=>tt(n,s.value)):tt(i,s.value)}return s.value===void 0?s:t.innerType._zod.run(s,r)}}),zi=l("$ZodExactOptional",(e,t)=>{Xt.init(e,t),g(e._zod,"values",()=>t.innerType._zod.values),g(e._zod,"pattern",()=>t.innerType._zod.pattern),e._zod.parse=(s,r)=>t.innerType._zod.run(s,r)}),Ei=l("$ZodNullable",(e,t)=>{S.init(e,t),g(e._zod,"optin",()=>t.innerType._zod.optin),g(e._zod,"optout",()=>t.innerType._zod.optout),g(e._zod,"pattern",()=>{const s=t.innerType._zod.pattern;return s?new RegExp(`^(${Ce(s.source)}|null)$`):void 0}),g(e._zod,"values",()=>t.innerType._zod.values?new Set([...t.innerType._zod.values,null]):void 0),e._zod.parse=(s,r)=>s.value===null?s:t.innerType._zod.run(s,r)}),xi=l("$ZodDefault",(e,t)=>{S.init(e,t),e._zod.optin="optional",g(e._zod,"values",()=>t.innerType._zod.values),e._zod.parse=(s,r)=>{if(r.direction==="backward")return t.innerType._zod.run(s,r);if(s.value===void 0)return s.value=t.defaultValue,s;const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(n=>st(n,t)):st(i,t)}});function st(e,t){return e.value===void 0&&(e.value=t.defaultValue),e}const Ai=l("$ZodPrefault",(e,t)=>{S.init(e,t),e._zod.optin="optional",g(e._zod,"values",()=>t.innerType._zod.values),e._zod.parse=(s,r)=>(r.direction==="backward"||s.value===void 0&&(s.value=t.defaultValue),t.innerType._zod.run(s,r))}),Oi=l("$ZodNonOptional",(e,t)=>{S.init(e,t),g(e._zod,"values",()=>{const s=t.innerType._zod.values;return s?new Set([...s].filter(r=>r!==void 0)):void 0}),e._zod.parse=(s,r)=>{const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(n=>rt(n,e)):rt(i,e)}});function rt(e,t){return!e.issues.length&&e.value===void 0&&e.issues.push({code:"invalid_type",expected:"nonoptional",input:e.value,inst:t}),e}const Pi=l("$ZodCatch",(e,t)=>{S.init(e,t),g(e._zod,"optin",()=>t.innerType._zod.optin),g(e._zod,"optout",()=>t.innerType._zod.optout),g(e._zod,"values",()=>t.innerType._zod.values),e._zod.parse=(s,r)=>{if(r.direction==="backward")return t.innerType._zod.run(s,r);const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(n=>(s.value=n.value,n.issues.length&&(s.value=t.catchValue({...s,error:{issues:n.issues.map(a=>J(a,r,G()))},input:s.value}),s.issues=[]),s)):(s.value=i.value,i.issues.length&&(s.value=t.catchValue({...s,error:{issues:i.issues.map(n=>J(n,r,G()))},input:s.value}),s.issues=[]),s)}}),Ti=l("$ZodPipe",(e,t)=>{S.init(e,t),g(e._zod,"values",()=>t.in._zod.values),g(e._zod,"optin",()=>t.in._zod.optin),g(e._zod,"optout",()=>t.out._zod.optout),g(e._zod,"propValues",()=>t.in._zod.propValues),e._zod.parse=(s,r)=>{if(r.direction==="backward"){const n=t.out._zod.run(s,r);return n instanceof Promise?n.then(a=>ve(a,t.in,r)):ve(n,t.in,r)}const i=t.in._zod.run(s,r);return i instanceof Promise?i.then(n=>ve(n,t.out,r)):ve(i,t.out,r)}});function ve(e,t,s){return e.issues.length?(e.aborted=!0,e):t._zod.run({value:e.value,issues:e.issues},s)}const Ui=l("$ZodReadonly",(e,t)=>{S.init(e,t),g(e._zod,"propValues",()=>t.innerType._zod.propValues),g(e._zod,"values",()=>t.innerType._zod.values),g(e._zod,"optin",()=>t.innerType?._zod?.optin),g(e._zod,"optout",()=>t.innerType?._zod?.optout),e._zod.parse=(s,r)=>{if(r.direction==="backward")return t.innerType._zod.run(s,r);const i=t.innerType._zod.run(s,r);return i instanceof Promise?i.then(it):it(i)}});function it(e){return e.value=Object.freeze(e.value),e}const Ni=l("$ZodCustom",(e,t)=>{$.init(e,t),S.init(e,t),e._zod.parse=(s,r)=>s,e._zod.check=s=>{const r=s.value,i=t.fn(r);if(i instanceof Promise)return i.then(n=>nt(n,s,r,e));nt(i,s,r,e)}});function nt(e,t,s,r){if(!e){const i={code:"custom",input:s,inst:r,path:[...r._zod.def.path??[]],continue:!r._zod.def.abort};r._zod.def.params&&(i.params=r._zod.def.params),t.issues.push(pe(i))}}var at;class Ri{constructor(){this._map=new WeakMap,this._idmap=new Map}add(t,...s){const r=s[0];return this._map.set(t,r),r&&typeof r=="object"&&"id"in r&&this._idmap.set(r.id,t),this}clear(){return this._map=new WeakMap,this._idmap=new Map,this}remove(t){const s=this._map.get(t);return s&&typeof s=="object"&&"id"in s&&this._idmap.delete(s.id),this._map.delete(t),this}get(t){const s=t._zod.parent;if(s){const r={...this.get(s)??{}};delete r.id;const i={...r,...this._map.get(t)};return Object.keys(i).length?i:void 0}return this._map.get(t)}has(t){return this._map.has(t)}}function qi(){return new Ri}(at=globalThis).__zod_globalRegistry??(at.__zod_globalRegistry=qi());const ue=globalThis.__zod_globalRegistry;function Mi(e,t){return new e({type:"string",...m(t)})}function $i(e,t){return new e({type:"string",format:"email",check:"string_format",abort:!1,...m(t)})}function ot(e,t){return new e({type:"string",format:"guid",check:"string_format",abort:!1,...m(t)})}function Ci(e,t){return new e({type:"string",format:"uuid",check:"string_format",abort:!1,...m(t)})}function Di(e,t){return new e({type:"string",format:"uuid",check:"string_format",abort:!1,version:"v4",...m(t)})}function Fi(e,t){return new e({type:"string",format:"uuid",check:"string_format",abort:!1,version:"v6",...m(t)})}function Vi(e,t){return new e({type:"string",format:"uuid",check:"string_format",abort:!1,version:"v7",...m(t)})}function ji(e,t){return new e({type:"string",format:"url",check:"string_format",abort:!1,...m(t)})}function Li(e,t){return new e({type:"string",format:"emoji",check:"string_format",abort:!1,...m(t)})}function Zi(e,t){return new e({type:"string",format:"nanoid",check:"string_format",abort:!1,...m(t)})}function Bi(e,t){return new e({type:"string",format:"cuid",check:"string_format",abort:!1,...m(t)})}function Gi(e,t){return new e({type:"string",format:"cuid2",check:"string_format",abort:!1,...m(t)})}function Ji(e,t){return new e({type:"string",format:"ulid",check:"string_format",abort:!1,...m(t)})}function Wi(e,t){return new e({type:"string",format:"xid",check:"string_format",abort:!1,...m(t)})}function Hi(e,t){return new e({type:"string",format:"ksuid",check:"string_format",abort:!1,...m(t)})}function Xi(e,t){return new e({type:"string",format:"ipv4",check:"string_format",abort:!1,...m(t)})}function Yi(e,t){return new e({type:"string",format:"ipv6",check:"string_format",abort:!1,...m(t)})}function Ki(e,t){return new e({type:"string",format:"cidrv4",check:"string_format",abort:!1,...m(t)})}function Qi(e,t){return new e({type:"string",format:"cidrv6",check:"string_format",abort:!1,...m(t)})}function en(e,t){return new e({type:"string",format:"base64",check:"string_format",abort:!1,...m(t)})}function tn(e,t){return new e({type:"string",format:"base64url",check:"string_format",abort:!1,...m(t)})}function sn(e,t){return new e({type:"string",format:"e164",check:"string_format",abort:!1,...m(t)})}function rn(e,t){return new e({type:"string",format:"jwt",check:"string_format",abort:!1,...m(t)})}function nn(e,t){return new e({type:"string",format:"datetime",check:"string_format",offset:!1,local:!1,precision:null,...m(t)})}function an(e,t){return new e({type:"string",format:"date",check:"string_format",...m(t)})}function on(e,t){return new e({type:"string",format:"time",check:"string_format",precision:null,...m(t)})}function cn(e,t){return new e({type:"string",format:"duration",check:"string_format",...m(t)})}function un(e,t){return new e({type:"number",checks:[],...m(t)})}function dn(e,t){return new e({type:"number",check:"number_format",abort:!1,format:"safeint",...m(t)})}function ln(e,t){return new e({type:"boolean",...m(t)})}function fn(e){return new e({type:"unknown"})}function hn(e,t){return new e({type:"never",...m(t)})}function ct(e,t){return new Lt({check:"less_than",...m(t),value:e,inclusive:!1})}function Ae(e,t){return new Lt({check:"less_than",...m(t),value:e,inclusive:!0})}function ut(e,t){return new Zt({check:"greater_than",...m(t),value:e,inclusive:!1})}function Oe(e,t){return new Zt({check:"greater_than",...m(t),value:e,inclusive:!0})}function dt(e,t){return new xr({check:"multiple_of",...m(t),value:e})}function Yt(e,t){return new Or({check:"max_length",...m(t),maximum:e})}function _e(e,t){return new Pr({check:"min_length",...m(t),minimum:e})}function Kt(e,t){return new Tr({check:"length_equals",...m(t),length:e})}function pn(e,t){return new Ur({check:"string_format",format:"regex",...m(t),pattern:e})}function mn(e){return new Nr({check:"string_format",format:"lowercase",...m(e)})}function wn(e){return new Rr({check:"string_format",format:"uppercase",...m(e)})}function bn(e,t){return new qr({check:"string_format",format:"includes",...m(t),includes:e})}function vn(e,t){return new Mr({check:"string_format",format:"starts_with",...m(t),prefix:e})}function gn(e,t){return new $r({check:"string_format",format:"ends_with",...m(t),suffix:e})}function oe(e){return new Cr({check:"overwrite",tx:e})}function yn(e){return oe(t=>t.normalize(e))}function _n(){return oe(e=>e.trim())}function kn(){return oe(e=>e.toLowerCase())}function In(){return oe(e=>e.toUpperCase())}function Sn(){return oe(e=>Us(e))}function zn(e,t,s){return new e({type:"array",element:t,...m(s)})}function En(e,t,s){return new e({type:"custom",check:"custom",fn:t,...m(s)})}function xn(e){const t=An(s=>(s.addIssue=r=>{if(typeof r=="string")s.issues.push(pe(r,s.value,t._zod.def));else{const i=r;i.fatal&&(i.continue=!1),i.code??(i.code="custom"),i.input??(i.input=s.value),i.inst??(i.inst=t),i.continue??(i.continue=!t._zod.def.abort),s.issues.push(pe(i))}},e(s.value,s)));return t}function An(e,t){const s=new $({check:"custom",...m(t)});return s._zod.check=e,s}function Qt(e){let t=e?.target??"draft-2020-12";return t==="draft-4"&&(t="draft-04"),t==="draft-7"&&(t="draft-07"),{processors:e.processors??{},metadataRegistry:e?.metadata??ue,target:t,unrepresentable:e?.unrepresentable??"throw",override:e?.override??(()=>{}),io:e?.io??"output",counter:0,seen:new Map,cycles:e?.cycles??"ref",reused:e?.reused??"inline",external:e?.external??void 0}}function P(e,t,s={path:[],schemaPath:[]}){var r;const i=e._zod.def,n=t.seen.get(e);if(n)return n.count++,s.schemaPath.includes(e)&&(n.cycle=s.path),n.schema;const a={schema:{},count:1,cycle:void 0,path:s.path};t.seen.set(e,a);const o=e._zod.toJSONSchema?.();if(o)a.schema=o;else{const u={...s,schemaPath:[...s.schemaPath,e],path:s.path};if(e._zod.processJSONSchema)e._zod.processJSONSchema(t,a.schema,u);else{const f=a.schema,h=t.processors[i.type];if(!h)throw new Error(`[toJSONSchema]: Non-representable type encountered: ${i.type}`);h(e,t,f,u)}const d=e._zod.parent;d&&(a.ref||(a.ref=d),P(d,t,u),t.seen.get(d).isParent=!0)}const c=t.metadataRegistry.get(e);return c&&Object.assign(a.schema,c),t.io==="input"&&N(e)&&(delete a.schema.examples,delete a.schema.default),t.io==="input"&&a.schema._prefault&&((r=a.schema).default??(r.default=a.schema._prefault)),delete a.schema._prefault,t.seen.get(e).schema}function es(e,t){const s=e.seen.get(t);if(!s)throw new Error("Unprocessed schema. This is a bug in Zod.");const r=new Map;for(const a of e.seen.entries()){const o=e.metadataRegistry.get(a[0])?.id;if(o){const c=r.get(o);if(c&&c!==a[0])throw new Error(`Duplicate schema id "${o}" detected during JSON Schema conversion. Two different schemas cannot share the same id when converted together.`);r.set(o,a[0])}}const i=a=>{const o=e.target==="draft-2020-12"?"$defs":"definitions";if(e.external){const d=e.external.registry.get(a[0])?.id,f=e.external.uri??(b=>b);if(d)return{ref:f(d)};const h=a[1].defId??a[1].schema.id??`schema${e.counter++}`;return a[1].defId=h,{defId:h,ref:`${f("__shared")}#/${o}/${h}`}}if(a[1]===s)return{ref:"#"};const c=`#/${o}/`,u=a[1].schema.id??`__schema${e.counter++}`;return{defId:u,ref:c+u}},n=a=>{if(a[1].schema.$ref)return;const o=a[1],{ref:c,defId:u}=i(a);o.def={...o.schema},u&&(o.defId=u);const d=o.schema;for(const f in d)delete d[f];d.$ref=c};if(e.cycles==="throw")for(const a of e.seen.entries()){const o=a[1];if(o.cycle)throw new Error(`Cycle detected: #/${o.cycle?.join("/")}/<root>

Set the \`cycles\` parameter to \`"ref"\` to resolve cyclical schemas with defs.`)}for(const a of e.seen.entries()){const o=a[1];if(t===a[0]){n(a);continue}if(e.external){const c=e.external.registry.get(a[0])?.id;if(t!==a[0]&&c){n(a);continue}}if(e.metadataRegistry.get(a[0])?.id){n(a);continue}if(o.cycle){n(a);continue}if(o.count>1&&e.reused==="ref"){n(a);continue}}}function ts(e,t){const s=e.seen.get(t);if(!s)throw new Error("Unprocessed schema. This is a bug in Zod.");const r=a=>{const o=e.seen.get(a);if(o.ref===null)return;const c=o.def??o.schema,u={...c},d=o.ref;if(o.ref=null,d){r(d);const h=e.seen.get(d),b=h.schema;if(b.$ref&&(e.target==="draft-07"||e.target==="draft-04"||e.target==="openapi-3.0")?(c.allOf=c.allOf??[],c.allOf.push(b)):Object.assign(c,b),Object.assign(c,u),a._zod.parent===d)for(const v in c)v==="$ref"||v==="allOf"||v in u||delete c[v];if(b.$ref&&h.def)for(const v in c)v==="$ref"||v==="allOf"||v in h.def&&JSON.stringify(c[v])===JSON.stringify(h.def[v])&&delete c[v]}const f=a._zod.parent;if(f&&f!==d){r(f);const h=e.seen.get(f);if(h?.schema.$ref&&(c.$ref=h.schema.$ref,h.def))for(const b in c)b==="$ref"||b==="allOf"||b in h.def&&JSON.stringify(c[b])===JSON.stringify(h.def[b])&&delete c[b]}e.override({zodSchema:a,jsonSchema:c,path:o.path??[]})};for(const a of[...e.seen.entries()].reverse())r(a[0]);const i={};if(e.target==="draft-2020-12"?i.$schema="https://json-schema.org/draft/2020-12/schema":e.target==="draft-07"?i.$schema="http://json-schema.org/draft-07/schema#":e.target==="draft-04"?i.$schema="http://json-schema.org/draft-04/schema#":e.target,e.external?.uri){const a=e.external.registry.get(t)?.id;if(!a)throw new Error("Schema is missing an `id` property");i.$id=e.external.uri(a)}Object.assign(i,s.def??s.schema);const n=e.external?.defs??{};for(const a of e.seen.entries()){const o=a[1];o.def&&o.defId&&(n[o.defId]=o.def)}e.external||Object.keys(n).length>0&&(e.target==="draft-2020-12"?i.$defs=n:i.definitions=n);try{const a=JSON.parse(JSON.stringify(i));return Object.defineProperty(a,"~standard",{value:{...t["~standard"],jsonSchema:{input:ke(t,"input",e.processors),output:ke(t,"output",e.processors)}},enumerable:!1,writable:!1}),a}catch{throw new Error("Error converting schema to JSON.")}}function N(e,t){const s=t??{seen:new Set};if(s.seen.has(e))return!1;s.seen.add(e);const r=e._zod.def;if(r.type==="transform")return!0;if(r.type==="array")return N(r.element,s);if(r.type==="set")return N(r.valueType,s);if(r.type==="lazy")return N(r.getter(),s);if(r.type==="promise"||r.type==="optional"||r.type==="nonoptional"||r.type==="nullable"||r.type==="readonly"||r.type==="default"||r.type==="prefault")return N(r.innerType,s);if(r.type==="intersection")return N(r.left,s)||N(r.right,s);if(r.type==="record"||r.type==="map")return N(r.keyType,s)||N(r.valueType,s);if(r.type==="pipe")return N(r.in,s)||N(r.out,s);if(r.type==="object"){for(const i in r.shape)if(N(r.shape[i],s))return!0;return!1}if(r.type==="union"){for(const i of r.options)if(N(i,s))return!0;return!1}if(r.type==="tuple"){for(const i of r.items)if(N(i,s))return!0;return!!(r.rest&&N(r.rest,s))}return!1}const On=(e,t={})=>s=>{const r=Qt({...s,processors:t});return P(e,r),es(r,e),ts(r,e)},ke=(e,t,s={})=>r=>{const{libraryOptions:i,target:n}=r??{},a=Qt({...i??{},target:n,io:t,processors:s});return P(e,a),es(a,e),ts(a,e)},Pn={guid:"uuid",url:"uri",datetime:"date-time",json_string:"json-string",regex:""},Tn=(e,t,s,r)=>{const i=s;i.type="string";const{minimum:n,maximum:a,format:o,patterns:c,contentEncoding:u}=e._zod.bag;if(typeof n=="number"&&(i.minLength=n),typeof a=="number"&&(i.maxLength=a),o&&(i.format=Pn[o]??o,i.format===""&&delete i.format,o==="time"&&delete i.format),u&&(i.contentEncoding=u),c&&c.size>0){const d=[...c];d.length===1?i.pattern=d[0].source:d.length>1&&(i.allOf=[...d.map(f=>({...t.target==="draft-07"||t.target==="draft-04"||t.target==="openapi-3.0"?{type:"string"}:{},pattern:f.source}))])}},Un=(e,t,s,r)=>{const i=s,{minimum:n,maximum:a,format:o,multipleOf:c,exclusiveMaximum:u,exclusiveMinimum:d}=e._zod.bag;typeof o=="string"&&o.includes("int")?i.type="integer":i.type="number",typeof d=="number"&&(t.target==="draft-04"||t.target==="openapi-3.0"?(i.minimum=d,i.exclusiveMinimum=!0):i.exclusiveMinimum=d),typeof n=="number"&&(i.minimum=n,typeof d=="number"&&t.target!=="draft-04"&&(d>=n?delete i.minimum:delete i.exclusiveMinimum)),typeof u=="number"&&(t.target==="draft-04"||t.target==="openapi-3.0"?(i.maximum=u,i.exclusiveMaximum=!0):i.exclusiveMaximum=u),typeof a=="number"&&(i.maximum=a,typeof u=="number"&&t.target!=="draft-04"&&(u<=a?delete i.maximum:delete i.exclusiveMaximum)),typeof c=="number"&&(i.multipleOf=c)},Nn=(e,t,s,r)=>{s.type="boolean"},Rn=(e,t,s,r)=>{s.not={}},qn=(e,t,s,r)=>{},Mn=(e,t,s,r)=>{const i=e._zod.def,n=Ut(i.entries);n.every(a=>typeof a=="number")&&(s.type="number"),n.every(a=>typeof a=="string")&&(s.type="string"),s.enum=n},$n=(e,t,s,r)=>{const i=e._zod.def,n=[];for(const a of i.values)if(a===void 0){if(t.unrepresentable==="throw")throw new Error("Literal `undefined` cannot be represented in JSON Schema")}else if(typeof a=="bigint"){if(t.unrepresentable==="throw")throw new Error("BigInt literals cannot be represented in JSON Schema");n.push(Number(a))}else n.push(a);if(n.length!==0)if(n.length===1){const a=n[0];s.type=a===null?"null":typeof a,t.target==="draft-04"||t.target==="openapi-3.0"?s.enum=[a]:s.const=a}else n.every(a=>typeof a=="number")&&(s.type="number"),n.every(a=>typeof a=="string")&&(s.type="string"),n.every(a=>typeof a=="boolean")&&(s.type="boolean"),n.every(a=>a===null)&&(s.type="null"),s.enum=n},Cn=(e,t,s,r)=>{if(t.unrepresentable==="throw")throw new Error("Custom types cannot be represented in JSON Schema")},Dn=(e,t,s,r)=>{if(t.unrepresentable==="throw")throw new Error("Transforms cannot be represented in JSON Schema")},Fn=(e,t,s,r)=>{const i=s,n=e._zod.def,{minimum:a,maximum:o}=e._zod.bag;typeof a=="number"&&(i.minItems=a),typeof o=="number"&&(i.maxItems=o),i.type="array",i.items=P(n.element,t,{...r,path:[...r.path,"items"]})},Vn=(e,t,s,r)=>{const i=s,n=e._zod.def;i.type="object",i.properties={};const a=n.shape;for(const u in a)i.properties[u]=P(a[u],t,{...r,path:[...r.path,"properties",u]});const o=new Set(Object.keys(a)),c=new Set([...o].filter(u=>{const d=n.shape[u]._zod;return t.io==="input"?d.optin===void 0:d.optout===void 0}));c.size>0&&(i.required=Array.from(c)),n.catchall?._zod.def.type==="never"?i.additionalProperties=!1:n.catchall?n.catchall&&(i.additionalProperties=P(n.catchall,t,{...r,path:[...r.path,"additionalProperties"]})):t.io==="output"&&(i.additionalProperties=!1)},jn=(e,t,s,r)=>{const i=e._zod.def,n=i.inclusive===!1,a=i.options.map((o,c)=>P(o,t,{...r,path:[...r.path,n?"oneOf":"anyOf",c]}));n?s.oneOf=a:s.anyOf=a},Ln=(e,t,s,r)=>{const i=e._zod.def,n=P(i.left,t,{...r,path:[...r.path,"allOf",0]}),a=P(i.right,t,{...r,path:[...r.path,"allOf",1]}),o=u=>"allOf"in u&&Object.keys(u).length===1,c=[...o(n)?n.allOf:[n],...o(a)?a.allOf:[a]];s.allOf=c},Zn=(e,t,s,r)=>{const i=s,n=e._zod.def;i.type="object";const a=n.keyType,o=a._zod.bag?.patterns;if(n.mode==="loose"&&o&&o.size>0){const u=P(n.valueType,t,{...r,path:[...r.path,"patternProperties","*"]});i.patternProperties={};for(const d of o)i.patternProperties[d.source]=u}else(t.target==="draft-07"||t.target==="draft-2020-12")&&(i.propertyNames=P(n.keyType,t,{...r,path:[...r.path,"propertyNames"]})),i.additionalProperties=P(n.valueType,t,{...r,path:[...r.path,"additionalProperties"]});const c=a._zod.values;if(c){const u=[...c].filter(d=>typeof d=="string"||typeof d=="number");u.length>0&&(i.required=u)}},Bn=(e,t,s,r)=>{const i=e._zod.def,n=P(i.innerType,t,r),a=t.seen.get(e);t.target==="openapi-3.0"?(a.ref=i.innerType,s.nullable=!0):s.anyOf=[n,{type:"null"}]},Gn=(e,t,s,r)=>{const i=e._zod.def;P(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType},Jn=(e,t,s,r)=>{const i=e._zod.def;P(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType,s.default=JSON.parse(JSON.stringify(i.defaultValue))},Wn=(e,t,s,r)=>{const i=e._zod.def;P(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType,t.io==="input"&&(s._prefault=JSON.parse(JSON.stringify(i.defaultValue)))},Hn=(e,t,s,r)=>{const i=e._zod.def;P(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType;let a;try{a=i.catchValue(void 0)}catch{throw new Error("Dynamic catch values are not supported in JSON Schema")}s.default=a},Xn=(e,t,s,r)=>{const i=e._zod.def,n=t.io==="input"?i.in._zod.def.type==="transform"?i.out:i.in:i.out;P(n,t,r);const a=t.seen.get(e);a.ref=n},Yn=(e,t,s,r)=>{const i=e._zod.def;P(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType,s.readOnly=!0},ss=(e,t,s,r)=>{const i=e._zod.def;P(i.innerType,t,r);const n=t.seen.get(e);n.ref=i.innerType},Kn=l("ZodISODateTime",(e,t)=>{Kr.init(e,t),E.init(e,t)});function Qn(e){return nn(Kn,e)}const ea=l("ZodISODate",(e,t)=>{Qr.init(e,t),E.init(e,t)});function ta(e){return an(ea,e)}const sa=l("ZodISOTime",(e,t)=>{ei.init(e,t),E.init(e,t)});function ra(e){return on(sa,e)}const ia=l("ZodISODuration",(e,t)=>{ti.init(e,t),E.init(e,t)});function na(e){return cn(ia,e)}const aa=(e,t)=>{Mt.init(e,t),e.name="ZodError",Object.defineProperties(e,{format:{value:s=>Bs(e,s)},flatten:{value:s=>Zs(e,s)},addIssue:{value:s=>{e.issues.push(s),e.message=JSON.stringify(e.issues,Ne,2)}},addIssues:{value:s=>{e.issues.push(...s),e.message=JSON.stringify(e.issues,Ne,2)}},isEmpty:{get(){return e.issues.length===0}}})},F=l("ZodError",aa,{Parent:Error}),oa=Fe(F),ca=Ve(F),ua=ze(F),da=Ee(F),la=Ws(F),fa=Hs(F),ha=Xs(F),pa=Ys(F),ma=Ks(F),wa=Qs(F),ba=er(F),va=tr(F),z=l("ZodType",(e,t)=>(S.init(e,t),Object.assign(e["~standard"],{jsonSchema:{input:ke(e,"input"),output:ke(e,"output")}}),e.toJSONSchema=On(e,{}),e.def=t,e.type=t.type,Object.defineProperty(e,"_def",{value:t}),e.check=(...s)=>e.clone(H(t,{checks:[...t.checks??[],...s.map(r=>typeof r=="function"?{_zod:{check:r,def:{check:"custom"},onattach:[]}}:r)]}),{parent:!0}),e.with=e.check,e.clone=(s,r)=>X(e,s,r),e.brand=()=>e,e.register=((s,r)=>(s.add(e,r),e)),e.parse=(s,r)=>oa(e,s,r,{callee:e.parse}),e.safeParse=(s,r)=>ua(e,s,r),e.parseAsync=async(s,r)=>ca(e,s,r,{callee:e.parseAsync}),e.safeParseAsync=async(s,r)=>da(e,s,r),e.spa=e.safeParseAsync,e.encode=(s,r)=>la(e,s,r),e.decode=(s,r)=>fa(e,s,r),e.encodeAsync=async(s,r)=>ha(e,s,r),e.decodeAsync=async(s,r)=>pa(e,s,r),e.safeEncode=(s,r)=>ma(e,s,r),e.safeDecode=(s,r)=>wa(e,s,r),e.safeEncodeAsync=async(s,r)=>ba(e,s,r),e.safeDecodeAsync=async(s,r)=>va(e,s,r),e.refine=(s,r)=>e.check(mo(s,r)),e.superRefine=s=>e.check(wo(s)),e.overwrite=s=>e.check(oe(s)),e.optional=()=>mt(e),e.exactOptional=()=>to(e),e.nullable=()=>wt(e),e.nullish=()=>mt(wt(e)),e.nonoptional=s=>oo(e,s),e.array=()=>B(e),e.or=s=>Za([e,s]),e.and=s=>Wa(e,s),e.transform=s=>bt(e,Qa(s)),e.default=s=>io(e,s),e.prefault=s=>ao(e,s),e.catch=s=>uo(e,s),e.pipe=s=>bt(e,s),e.readonly=()=>ho(e),e.describe=s=>{const r=e.clone();return ue.add(r,{description:s}),r},Object.defineProperty(e,"description",{get(){return ue.get(e)?.description},configurable:!0}),e.meta=(...s)=>{if(s.length===0)return ue.get(e);const r=e.clone();return ue.add(r,s[0]),r},e.isOptional=()=>e.safeParse(void 0).success,e.isNullable=()=>e.safeParse(null).success,e.apply=s=>s(e),e)),rs=l("_ZodString",(e,t)=>{je.init(e,t),z.init(e,t),e._zod.processJSONSchema=(r,i,n)=>Tn(e,r,i);const s=e._zod.bag;e.format=s.format??null,e.minLength=s.minimum??null,e.maxLength=s.maximum??null,e.regex=(...r)=>e.check(pn(...r)),e.includes=(...r)=>e.check(bn(...r)),e.startsWith=(...r)=>e.check(vn(...r)),e.endsWith=(...r)=>e.check(gn(...r)),e.min=(...r)=>e.check(_e(...r)),e.max=(...r)=>e.check(Yt(...r)),e.length=(...r)=>e.check(Kt(...r)),e.nonempty=(...r)=>e.check(_e(1,...r)),e.lowercase=r=>e.check(mn(r)),e.uppercase=r=>e.check(wn(r)),e.trim=()=>e.check(_n()),e.normalize=(...r)=>e.check(yn(...r)),e.toLowerCase=()=>e.check(kn()),e.toUpperCase=()=>e.check(In()),e.slugify=()=>e.check(Sn())}),ga=l("ZodString",(e,t)=>{je.init(e,t),rs.init(e,t),e.email=s=>e.check($i(ya,s)),e.url=s=>e.check(ji(_a,s)),e.jwt=s=>e.check(rn(Ma,s)),e.emoji=s=>e.check(Li(ka,s)),e.guid=s=>e.check(ot(lt,s)),e.uuid=s=>e.check(Ci(ge,s)),e.uuidv4=s=>e.check(Di(ge,s)),e.uuidv6=s=>e.check(Fi(ge,s)),e.uuidv7=s=>e.check(Vi(ge,s)),e.nanoid=s=>e.check(Zi(Ia,s)),e.guid=s=>e.check(ot(lt,s)),e.cuid=s=>e.check(Bi(Sa,s)),e.cuid2=s=>e.check(Gi(za,s)),e.ulid=s=>e.check(Ji(Ea,s)),e.base64=s=>e.check(en(Na,s)),e.base64url=s=>e.check(tn(Ra,s)),e.xid=s=>e.check(Wi(xa,s)),e.ksuid=s=>e.check(Hi(Aa,s)),e.ipv4=s=>e.check(Xi(Oa,s)),e.ipv6=s=>e.check(Yi(Pa,s)),e.cidrv4=s=>e.check(Ki(Ta,s)),e.cidrv6=s=>e.check(Qi(Ua,s)),e.e164=s=>e.check(sn(qa,s)),e.datetime=s=>e.check(Qn(s)),e.date=s=>e.check(ta(s)),e.time=s=>e.check(ra(s)),e.duration=s=>e.check(na(s))});function I(e){return Mi(ga,e)}const E=l("ZodStringFormat",(e,t)=>{_.init(e,t),rs.init(e,t)}),ya=l("ZodEmail",(e,t)=>{Lr.init(e,t),E.init(e,t)}),lt=l("ZodGUID",(e,t)=>{Vr.init(e,t),E.init(e,t)}),ge=l("ZodUUID",(e,t)=>{jr.init(e,t),E.init(e,t)}),_a=l("ZodURL",(e,t)=>{Zr.init(e,t),E.init(e,t)}),ka=l("ZodEmoji",(e,t)=>{Br.init(e,t),E.init(e,t)}),Ia=l("ZodNanoID",(e,t)=>{Gr.init(e,t),E.init(e,t)}),Sa=l("ZodCUID",(e,t)=>{Jr.init(e,t),E.init(e,t)}),za=l("ZodCUID2",(e,t)=>{Wr.init(e,t),E.init(e,t)}),Ea=l("ZodULID",(e,t)=>{Hr.init(e,t),E.init(e,t)}),xa=l("ZodXID",(e,t)=>{Xr.init(e,t),E.init(e,t)}),Aa=l("ZodKSUID",(e,t)=>{Yr.init(e,t),E.init(e,t)}),Oa=l("ZodIPv4",(e,t)=>{si.init(e,t),E.init(e,t)}),Pa=l("ZodIPv6",(e,t)=>{ri.init(e,t),E.init(e,t)}),Ta=l("ZodCIDRv4",(e,t)=>{ii.init(e,t),E.init(e,t)}),Ua=l("ZodCIDRv6",(e,t)=>{ni.init(e,t),E.init(e,t)}),Na=l("ZodBase64",(e,t)=>{ai.init(e,t),E.init(e,t)}),Ra=l("ZodBase64URL",(e,t)=>{ci.init(e,t),E.init(e,t)}),qa=l("ZodE164",(e,t)=>{ui.init(e,t),E.init(e,t)}),Ma=l("ZodJWT",(e,t)=>{li.init(e,t),E.init(e,t)}),is=l("ZodNumber",(e,t)=>{Gt.init(e,t),z.init(e,t),e._zod.processJSONSchema=(r,i,n)=>Un(e,r,i),e.gt=(r,i)=>e.check(ut(r,i)),e.gte=(r,i)=>e.check(Oe(r,i)),e.min=(r,i)=>e.check(Oe(r,i)),e.lt=(r,i)=>e.check(ct(r,i)),e.lte=(r,i)=>e.check(Ae(r,i)),e.max=(r,i)=>e.check(Ae(r,i)),e.int=r=>e.check(ft(r)),e.safe=r=>e.check(ft(r)),e.positive=r=>e.check(ut(0,r)),e.nonnegative=r=>e.check(Oe(0,r)),e.negative=r=>e.check(ct(0,r)),e.nonpositive=r=>e.check(Ae(0,r)),e.multipleOf=(r,i)=>e.check(dt(r,i)),e.step=(r,i)=>e.check(dt(r,i)),e.finite=()=>e;const s=e._zod.bag;e.minValue=Math.max(s.minimum??Number.NEGATIVE_INFINITY,s.exclusiveMinimum??Number.NEGATIVE_INFINITY)??null,e.maxValue=Math.min(s.maximum??Number.POSITIVE_INFINITY,s.exclusiveMaximum??Number.POSITIVE_INFINITY)??null,e.isInt=(s.format??"").includes("int")||Number.isSafeInteger(s.multipleOf??.5),e.isFinite=!0,e.format=s.format??null});function Z(e){return un(is,e)}const $a=l("ZodNumberFormat",(e,t)=>{fi.init(e,t),is.init(e,t)});function ft(e){return dn($a,e)}const Ca=l("ZodBoolean",(e,t)=>{hi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Nn(e,s,r)});function Y(e){return ln(Ca,e)}const Da=l("ZodUnknown",(e,t)=>{pi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>qn()});function ht(){return fn(Da)}const Fa=l("ZodNever",(e,t)=>{mi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Rn(e,s,r)});function Va(e){return hn(Fa,e)}const ja=l("ZodArray",(e,t)=>{wi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Fn(e,s,r,i),e.element=t.element,e.min=(s,r)=>e.check(_e(s,r)),e.nonempty=s=>e.check(_e(1,s)),e.max=(s,r)=>e.check(Yt(s,r)),e.length=(s,r)=>e.check(Kt(s,r)),e.unwrap=()=>e.element});function B(e,t){return zn(ja,e,t)}const La=l("ZodObject",(e,t)=>{vi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Vn(e,s,r,i),g(e,"shape",()=>t.shape),e.keyof=()=>Xa(Object.keys(e._zod.def.shape)),e.catchall=s=>e.clone({...e._zod.def,catchall:s}),e.passthrough=()=>e.clone({...e._zod.def,catchall:ht()}),e.loose=()=>e.clone({...e._zod.def,catchall:ht()}),e.strict=()=>e.clone({...e._zod.def,catchall:Va()}),e.strip=()=>e.clone({...e._zod.def,catchall:void 0}),e.extend=s=>Ds(e,s),e.safeExtend=s=>Fs(e,s),e.merge=s=>Vs(e,s),e.pick=s=>$s(e,s),e.omit=s=>Cs(e,s),e.partial=(...s)=>js(as,e,s[0]),e.required=(...s)=>Ls(os,e,s[0])});function A(e,t){const s={type:"object",shape:e??{},...m(t)};return new La(s)}const ns=l("ZodUnion",(e,t)=>{Ht.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>jn(e,s,r,i),e.options=t.options});function Za(e,t){return new ns({type:"union",options:e,...m(t)})}const Ba=l("ZodDiscriminatedUnion",(e,t)=>{ns.init(e,t),gi.init(e,t)});function Ga(e,t,s){return new Ba({type:"union",options:t,discriminator:e,...m(s)})}const Ja=l("ZodIntersection",(e,t)=>{yi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Ln(e,s,r,i)});function Wa(e,t){return new Ja({type:"intersection",left:e,right:t})}const Ha=l("ZodRecord",(e,t)=>{_i.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Zn(e,s,r,i),e.keyType=t.keyType,e.valueType=t.valueType});function Le(e,t,s){return new Ha({type:"record",keyType:e,valueType:t,...m(s)})}const qe=l("ZodEnum",(e,t)=>{ki.init(e,t),z.init(e,t),e._zod.processJSONSchema=(r,i,n)=>Mn(e,r,i),e.enum=t.entries,e.options=Object.values(t.entries);const s=new Set(Object.keys(t.entries));e.extract=(r,i)=>{const n={};for(const a of r)if(s.has(a))n[a]=t.entries[a];else throw new Error(`Key ${a} not found in enum`);return new qe({...t,checks:[],...m(i),entries:n})},e.exclude=(r,i)=>{const n={...t.entries};for(const a of r)if(s.has(a))delete n[a];else throw new Error(`Key ${a} not found in enum`);return new qe({...t,checks:[],...m(i),entries:n})}});function Xa(e,t){const s=Array.isArray(e)?Object.fromEntries(e.map(r=>[r,r])):e;return new qe({type:"enum",entries:s,...m(t)})}const Ya=l("ZodLiteral",(e,t)=>{Ii.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>$n(e,s,r),e.values=new Set(t.values),Object.defineProperty(e,"value",{get(){if(t.values.length>1)throw new Error("This schema contains multiple valid literal values. Use `.values` instead.");return t.values[0]}})});function pt(e,t){return new Ya({type:"literal",values:Array.isArray(e)?e:[e],...m(t)})}const Ka=l("ZodTransform",(e,t)=>{Si.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Dn(e,s),e._zod.parse=(s,r)=>{if(r.direction==="backward")throw new Pt(e.constructor.name);s.addIssue=n=>{if(typeof n=="string")s.issues.push(pe(n,s.value,t));else{const a=n;a.fatal&&(a.continue=!1),a.code??(a.code="custom"),a.input??(a.input=s.value),a.inst??(a.inst=e),s.issues.push(pe(a))}};const i=t.transform(s.value,s);return i instanceof Promise?i.then(n=>(s.value=n,s)):(s.value=i,s)}});function Qa(e){return new Ka({type:"transform",transform:e})}const as=l("ZodOptional",(e,t)=>{Xt.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>ss(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function mt(e){return new as({type:"optional",innerType:e})}const eo=l("ZodExactOptional",(e,t)=>{zi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>ss(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function to(e){return new eo({type:"optional",innerType:e})}const so=l("ZodNullable",(e,t)=>{Ei.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Bn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function wt(e){return new so({type:"nullable",innerType:e})}const ro=l("ZodDefault",(e,t)=>{xi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Jn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType,e.removeDefault=e.unwrap});function io(e,t){return new ro({type:"default",innerType:e,get defaultValue(){return typeof t=="function"?t():Rt(t)}})}const no=l("ZodPrefault",(e,t)=>{Ai.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Wn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function ao(e,t){return new no({type:"prefault",innerType:e,get defaultValue(){return typeof t=="function"?t():Rt(t)}})}const os=l("ZodNonOptional",(e,t)=>{Oi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Gn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function oo(e,t){return new os({type:"nonoptional",innerType:e,...m(t)})}const co=l("ZodCatch",(e,t)=>{Pi.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Hn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType,e.removeCatch=e.unwrap});function uo(e,t){return new co({type:"catch",innerType:e,catchValue:typeof t=="function"?t:()=>t})}const lo=l("ZodPipe",(e,t)=>{Ti.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Xn(e,s,r,i),e.in=t.in,e.out=t.out});function bt(e,t){return new lo({type:"pipe",in:e,out:t})}const fo=l("ZodReadonly",(e,t)=>{Ui.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Yn(e,s,r,i),e.unwrap=()=>e._zod.def.innerType});function ho(e){return new fo({type:"readonly",innerType:e})}const po=l("ZodCustom",(e,t)=>{Ni.init(e,t),z.init(e,t),e._zod.processJSONSchema=(s,r,i)=>Cn(e,s)});function mo(e,t={}){return En(po,e,t)}function wo(e){return xn(e)}Z().int().nonnegative().max(255).brand("u8");const M=Z().int().nonnegative().max(Number.MAX_SAFE_INTEGER).brand("u53"),cs=Ga("kind",[A({kind:pt("legacy")}),A({kind:pt("cmaf"),timescale:M,trackId:M})]).default({kind:"legacy"}),bo=A({name:I()}),vt=A({codec:I(),container:cs,description:I().optional(),sampleRate:M,numberOfChannels:M,bitrate:M.optional(),jitter:M.optional()}),vo=A({renditions:Le(I(),vt)}).or(A({track:bo,config:vt}).transform(e=>({renditions:{[e.track.name]:e.config}}))),go=A({hardware:B(I()).optional(),software:B(I()).optional(),unsupported:B(I()).optional()}),yo=A({hardware:B(I()).optional(),software:B(I()).optional(),unsupported:B(I()).optional()}),_o=A({video:go.optional(),audio:yo.optional()}),me=A({name:I()}),ko=A({message:me.optional(),typing:me.optional()}),us=A({x:Z().optional(),y:Z().optional(),z:Z().optional(),s:Z().optional()}),Io=A({initial:us.optional(),track:me.optional(),handle:I().optional(),peers:me.optional()});Le(I(),us);A({name:I().optional(),avatar:I().optional(),audio:Y().optional(),video:Y().optional(),typing:Y().optional(),chat:Y().optional(),screen:Y().optional()});const ae={catalog:100,audio:80,video:60},So=A({id:I().optional(),name:I().optional(),avatar:I().optional(),color:I().optional()}),zo=A({name:I()}),gt=A({codec:I(),container:cs,description:I().optional(),codedWidth:M.optional(),codedHeight:M.optional(),displayAspectWidth:M.optional(),displayAspectHeight:M.optional(),framerate:Z().optional(),bitrate:M.optional(),optimizeForLatency:Y().optional(),jitter:M.optional()}),Eo=A({renditions:Le(I(),gt),display:A({width:M,height:M}).optional(),rotation:Z().optional(),flip:Y().optional()}).or(B(A({track:zo,config:gt})).transform(e=>{const t=e[0]?.config;return{renditions:Object.fromEntries(e.map(s=>[s.track.name,s.config])),display:t?.displayAspectWidth&&t?.displayAspectHeight?{width:t.displayAspectWidth,height:t.displayAspectHeight}:void 0,rotation:void 0,flip:void 0}})),xo=A({video:Eo.optional(),audio:vo.optional(),location:Io.optional(),user:So.optional(),chat:ko.optional(),capabilities:_o.optional(),preview:me.optional()});function Ao(e){const t=new TextDecoder().decode(e);try{const s=JSON.parse(t);return xo.parse(s)}catch(s){throw console.warn("invalid catalog",t),s}}async function Oo(e){const t=await e.readFrame();if(t)return Ao(t)}function ds(e){return e instanceof ArrayBuffer||typeof SharedArrayBuffer<"u"&&e instanceof SharedArrayBuffer}const Po="utf-16",Pe="utf-16be",yt="utf-16le",le="utf-8";function ls(e,t={}){let s;ds(e)?s=new DataView(e):s=new DataView(e.buffer,e.byteOffset,e.byteLength);let r=0,{encoding:i}=t;if(!i){const u=s.getUint8(0),d=s.getUint8(1);u==239&&d==187&&s.getUint8(2)==191?(i=le,r=3):u==254&&d==255?(i=Pe,r=2):u==255&&d==254?(i=yt,r=2):i=le}if(typeof TextDecoder<"u")return new TextDecoder(i).decode(s);const{byteLength:n}=s,a=i!==Pe;let o="",c;for(;r<n;){switch(i){case le:if(c=s.getUint8(r),c<128)r++;else if(c>=194&&c<=223)if(r+1<n){const u=s.getUint8(r+1);u>=128&&u<=191?(c=(c&31)<<6|u&63,r+=2):r++}else r++;else if(c>=224&&c<=239)if(r+2<=n-1){const u=s.getUint8(r+1),d=s.getUint8(r+2);u>=128&&u<=191&&d>=128&&d<=191?(c=(c&15)<<12|(u&63)<<6|d&63,r+=3):r++}else r++;else if(c>=240&&c<=244)if(r+3<=n-1){const u=s.getUint8(r+1),d=s.getUint8(r+2),f=s.getUint8(r+3);u>=128&&u<=191&&d>=128&&d<=191&&f>=128&&f<=191?(c=(c&7)<<18|(u&63)<<12|(d&63)<<6|f&63,r+=4):r++}else r++;else r++;break;case Pe:case Po:case yt:c=s.getUint16(r,a),r+=2;break}o+=String.fromCodePoint(c)}return o}function To(e){return new TextEncoder().encode(e)}function Uo(e){return{writers:e?.writers??{}}}const No=["dinf","edts","grpl","mdia","meco","mfra","minf","moof","moov","mvex","schi","sinf","stbl","strk","traf","trak","tref","udta","vttc"];function fs(e){return"boxes"in e||No.includes(e.type)}const _t="utf8",W="uint",we="template",kt="string",It="int",St="data";var T=class{constructor(e,t){this.writeUint=(s,r)=>{const{dataView:i,cursor:n}=this;switch(r){case 1:i.setUint8(n,s);break;case 2:i.setUint16(n,s);break;case 3:{const a=(s&16776960)>>8,o=s&255;i.setUint16(n,a),i.setUint8(n+2,o);break}case 4:i.setUint32(n,s);break;case 8:{const a=Math.floor(s/Math.pow(2,32)),o=s-a*Math.pow(2,32);i.setUint32(n,a),i.setUint32(n+4,o);break}}this.cursor+=r},this.writeInt=(s,r)=>{const{dataView:i,cursor:n}=this;switch(r){case 1:i.setInt8(n,s);break;case 2:i.setInt16(n,s);break;case 4:i.setInt32(n,s);break;case 8:const a=Math.floor(s/Math.pow(2,32)),o=s-a*Math.pow(2,32);i.setUint32(n,a),i.setUint32(n+4,o);break}this.cursor+=r},this.writeString=s=>{for(let r=0,i=s.length;r<i;r++)this.writeUint(s.charCodeAt(r),1)},this.writeTerminatedString=s=>{if(s.length!==0){for(let r=0,i=s.length;r<i;r++)this.writeUint(s.charCodeAt(r),1);this.writeUint(0,1)}},this.writeUtf8TerminatedString=s=>{const r=To(s);new Uint8Array(this.dataView.buffer).set(r,this.cursor),this.cursor+=r.length,this.writeUint(0,1)},this.writeBytes=s=>{Array.isArray(s)||(s=[s]);for(const r of s)new Uint8Array(this.dataView.buffer).set(r,this.cursor),this.cursor+=r.length},this.writeArray=(s,r,i,n)=>{const a=r===W?this.writeUint:r===we?this.writeTemplate:this.writeInt;for(let o=0;o<n;o++)a(s[o]??0,i)},this.writeTemplate=(s,r)=>{const i=Math.round(s*Math.pow(2,r===4?16:8));this.writeUint(i,r)},this.writeBoxHeader=(s,r)=>{r>4294967295?(this.writeUint(1,4),this.writeString(s),this.writeUint(r,8)):(this.writeUint(r,4),this.writeString(s))},this.dataView=new DataView(new ArrayBuffer(t)),this.cursor=0,this.writeBoxHeader(e,t)}get buffer(){return this.dataView.buffer}get byteLength(){return this.dataView.byteLength}get byteOffset(){return this.dataView.byteOffset}writeFullBox(e,t){this.writeUint(e,1),this.writeUint(t,3)}};function hs(e,t){return Array.from(e,s=>qo(s,t))}function Ze(e,t){const s=hs(e,t);return{bytes:s,size:s.reduce((r,i)=>r+i.byteLength,0)}}function Ro(e,t){const{bytes:s,size:r}=Ze(e.boxes,t),i=8+r,n=new T(e.type,i);return n.writeBytes(s),n}function qo(e,t){let s=null;if("type"in e){const{type:r}=e,i=t.writers?.[r];if(i?s=i(e,t):fs(e)?s=Ro(e,t):"view"in e&&(s=e.view),!s)throw new Error(`No writer found for box type: ${r}`)}if("buffer"in e&&(s=e),!s)throw new Error("Invalid box");return new Uint8Array(s.buffer,s.byteOffset,s.byteLength)}function Mo(e,t,s){const r=s>0?s:e.byteLength-(t-e.byteOffset);return new Uint8Array(e.buffer,t,Math.max(r,0))}function $o(e,t,s){let r=NaN;const i=t-e.byteOffset;switch(s){case 1:r=e.getInt8(i);break;case 2:r=e.getInt16(i);break;case 4:r=e.getInt32(i);break;case 8:const n=e.getInt32(i),a=e.getInt32(i+4);r=n*Math.pow(2,32)+a;break}return r}function K(e,t,s){const r=t-e.byteOffset;let i=NaN,n,a;switch(s){case 1:i=e.getUint8(r);break;case 2:i=e.getUint16(r);break;case 3:n=e.getUint16(r),a=e.getUint8(r+2),i=(n<<8)+a;break;case 4:i=e.getUint32(r);break;case 8:n=e.getUint32(r),a=e.getUint32(r+4),i=n*Math.pow(2,32)+a;break}return i}function zt(e,t,s){let r="";for(let i=0;i<s;i++){const n=K(e,t+i,1);r+=String.fromCharCode(n)}return r}function Co(e,t,s){const r=s/2;return K(e,t,r)+K(e,t+r,r)/Math.pow(2,r)}function Do(e,t){let s="",r=t;for(;r-e.byteOffset<e.byteLength;){const i=K(e,r,1);if(i===0)break;s+=String.fromCharCode(i),r++}return s}function Fo(e,t){const s=e.byteLength-(t-e.byteOffset);return s>0?ls(new DataView(e.buffer,t,s),{encoding:le}):""}function Vo(e,t){const s=e.byteLength-(t-e.byteOffset);let r="";if(s>0){const i=new DataView(e.buffer,t,s);let n=0;for(;n<s&&i.getUint8(n)!==0;n++);r=ls(new DataView(e.buffer,t,n),{encoding:le})}return r}var jo=class ps{constructor(t,s){this.truncated=!1,this.slice=(r,i)=>{const n=new ps(new DataView(this.dataView.buffer,r,i),this.config),a=this.offset-r,o=i-a;return this.offset+=o,n.jump(a),n},this.read=(r,i=0)=>{const{dataView:n,offset:a}=this;let o,c=i;switch(r){case W:o=K(n,a,i);break;case It:o=$o(n,a,i);break;case we:o=Co(n,a,i);break;case kt:i===-1?(o=Do(n,a),c=o.length+1):o=zt(n,a,i);break;case St:o=Mo(n,a,i),c=o.length;break;case _t:i===-1?(o=Vo(n,a),c=o.length+1):o=Fo(n,a);break;default:o=-1}return this.offset+=c,o},this.readUint=r=>this.read(W,r),this.readInt=r=>this.read(It,r),this.readString=r=>this.read(kt,r),this.readTemplate=r=>this.read(we,r),this.readData=r=>this.read(St,r),this.readUtf8=r=>this.read(_t,r),this.readFullBox=()=>({version:this.readUint(1),flags:this.readUint(3)}),this.readArray=(r,i,n)=>{const a=[];for(let o=0;o<n;o++)a.push(this.read(r,i));return a},this.jump=r=>{this.offset+=r},this.readBox=()=>{const{dataView:r,offset:i}=this;let n=0;const a=K(r,i,4),o=zt(r,i+4,4),c={size:a,type:o};n+=8,c.size===1&&(c.largesize=K(r,i+n,8),n+=8);const u=c.size===0?this.bytesRemaining:c.largesize??c.size;if(this.cursor+u>r.byteLength)throw this.truncated=!0,new Error("Truncated box");return this.jump(n),o==="uuid"&&(c.usertype=this.readArray("uint",1,16)),c.view=this.slice(i,u),c},this.readBoxes=(r=-1)=>{const i=[];for(const n of this)if(i.push(n),r>0&&i.length>=r)break;return i},this.readEntries=(r,i)=>{const n=[];for(let a=0;a<r;a++)n.push(i());return n},this.dataView=ds(t)?new DataView(t):t instanceof DataView?t:new DataView(t.buffer,t.byteOffset,t.byteLength),this.offset=this.dataView.byteOffset,this.config=s||{}}get buffer(){return this.dataView.buffer}get byteOffset(){return this.dataView.byteOffset}get byteLength(){return this.dataView.byteLength}get cursor(){return this.offset-this.dataView.byteOffset}get done(){return this.cursor>=this.dataView.byteLength||this.truncated}get bytesRemaining(){return this.dataView.byteLength-this.cursor}*[Symbol.iterator](){const{readers:t={}}=this.config;for(;!this.done;)try{const s=this.readBox(),{type:r,view:i}=s,n=t[r]||t[r.trim()];if(n&&Object.assign(s,n(i,r)),fs(s)&&!s.boxes){const a=[];for(const o of i)a.push(o);s.boxes=a}yield s}catch(s){if(s instanceof Error&&s.message==="Truncated box")break;throw s}}};function ms(e,t){const s=[];for(const r of new jo(e,t))s.push(r);return s}function Lo(e,t){return hs(e,Uo(t))}function Zo(e){return{type:"mdat",data:e.readData(-1)}}function Bo(e){return{type:"mfhd",...e.readFullBox(),sequenceNumber:e.readUint(4)}}function Go(e){const{version:t,flags:s}=e.readFullBox();return{type:"tfdt",version:t,flags:s,baseMediaDecodeTime:e.readUint(t==1?8:4)}}function Jo(e){const{version:t,flags:s}=e.readFullBox();return{type:"tfhd",version:t,flags:s,trackId:e.readUint(4),baseDataOffset:s&1?e.readUint(8):void 0,sampleDescriptionIndex:s&2?e.readUint(4):void 0,defaultSampleDuration:s&8?e.readUint(4):void 0,defaultSampleSize:s&16?e.readUint(4):void 0,defaultSampleFlags:s&32?e.readUint(4):void 0}}function Wo(e){const{version:t,flags:s}=e.readFullBox(),r=e.readUint(4);let i,n;s&1&&(i=e.readInt(4)),s&4&&(n=e.readUint(4));const a=e.readEntries(r,()=>{const o={};return s&256&&(o.sampleDuration=e.readUint(4)),s&512&&(o.sampleSize=e.readUint(4)),s&1024&&(o.sampleFlags=e.readUint(4)),s&2048&&(o.sampleCompositionTimeOffset=t===1?e.readInt(4):e.readUint(4)),o});return{type:"trun",version:t,flags:s,sampleCount:r,dataOffset:i,firstSampleFlags:n,samples:a}}function Ho(e,t){const s=e.entries.length,{bytes:r,size:i}=Ze(e.entries,t),n=new T("dref",16+i);return n.writeFullBox(e.version,e.flags),n.writeUint(s,4),n.writeBytes(r),n}function Xo(e){const t=e.compatibleBrands.length*4,s=new T("ftyp",16+t);s.writeString(e.majorBrand),s.writeUint(e.minorVersion,4);for(const r of e.compatibleBrands)s.writeString(r);return s}function Yo(e){const t=e.name.length+1,s=new T("hdlr",32+t);return s.writeFullBox(e.version,e.flags),s.writeUint(e.preDefined,4),s.writeString(e.handlerType),s.writeArray(e.reserved,W,4,3),s.writeTerminatedString(e.name),s}function Ko(e){const t=new T("mdat",8+e.data.length);return t.writeBytes(e.data),t}function Qo(e){const t=e.version===1?8:4,s=8,r=4,i=t*3,n=new T("mdhd",s+r+i+4+2+2);n.writeFullBox(e.version,e.flags),n.writeUint(e.creationTime,t),n.writeUint(e.modificationTime,t),n.writeUint(e.timescale,4),n.writeUint(e.duration,t);const a=e.language.length>=3?(e.language.charCodeAt(0)-96&31)<<10|(e.language.charCodeAt(1)-96&31)<<5|e.language.charCodeAt(2)-96&31:0;return n.writeUint(a,2),n.writeUint(e.preDefined,2),n}function ec(e){const t=new T("mfhd",16);return t.writeFullBox(e.version,e.flags),t.writeUint(e.sequenceNumber,4),t}function tc(e){const t=e.version===1?8:4,s=8,r=4,i=t*3,n=new T("mvhd",s+r+i+4+4+2+2+8+36+24+4);return n.writeFullBox(e.version,e.flags),n.writeUint(e.creationTime,t),n.writeUint(e.modificationTime,t),n.writeUint(e.timescale,4),n.writeUint(e.duration,t),n.writeTemplate(e.rate,4),n.writeTemplate(e.volume,2),n.writeUint(e.reserved1,2),n.writeArray(e.reserved2,W,4,2),n.writeArray(e.matrix,we,4,9),n.writeArray(e.preDefined,W,4,6),n.writeUint(e.nextTrackId,4),n}function sc(e){const t=new T("smhd",16);return t.writeFullBox(e.version,e.flags),t.writeUint(e.balance,2),t.writeUint(e.reserved,2),t}function rc(e,t){const s=e.entries.length,{bytes:r,size:i}=Ze(e.entries,t),n=new T("stsd",16+i);return n.writeFullBox(e.version,e.flags),n.writeUint(s,4),n.writeBytes(r),n}function ic(e){const t=e.entryCount*8,s=new T("stts",16+t);s.writeFullBox(e.version,e.flags),s.writeUint(e.entryCount,4);for(const r of e.entries)s.writeUint(r.sampleCount,4),s.writeUint(r.sampleDelta,4);return s}function nc(e){const t=e.version===1?8:4,s=8,r=4,i=t,n=new T("tfdt",s+r+i);return n.writeFullBox(e.version,e.flags),n.writeUint(e.baseMediaDecodeTime,t),n}function ac(e){const t=e.flags&1?8:0,s=e.flags&2?4:0,r=e.flags&8?4:0,i=e.flags&16?4:0,n=e.flags&32?4:0,a=new T("tfhd",16+t+s+r+i+n);return a.writeFullBox(e.version,e.flags),a.writeUint(e.trackId,4),e.flags&1&&a.writeUint(e.baseDataOffset??0,8),e.flags&2&&a.writeUint(e.sampleDescriptionIndex??0,4),e.flags&8&&a.writeUint(e.defaultSampleDuration??0,4),e.flags&16&&a.writeUint(e.defaultSampleSize??0,4),e.flags&32&&a.writeUint(e.defaultSampleFlags??0,4),a}function oc(e){const t=e.version===1?8:4,s=8,r=4,i=t*3,n=new T("tkhd",s+r+i+4+4+8+2+2+2+2+36+4+4);return n.writeFullBox(e.version,e.flags),n.writeUint(e.creationTime,t),n.writeUint(e.modificationTime,t),n.writeUint(e.trackId,4),n.writeUint(e.reserved1,4),n.writeUint(e.duration,t),n.writeArray(e.reserved2,W,4,2),n.writeUint(e.layer,2),n.writeUint(e.alternateGroup,2),n.writeTemplate(e.volume,2),n.writeUint(e.reserved3,2),n.writeArray(e.matrix,we,4,9),n.writeTemplate(e.width,4),n.writeTemplate(e.height,4),n}function cc(e){const t=new T("trex",32);return t.writeFullBox(e.version,e.flags),t.writeUint(e.trackId,4),t.writeUint(e.defaultSampleDescriptionIndex,4),t.writeUint(e.defaultSampleDuration,4),t.writeUint(e.defaultSampleSize,4),t.writeUint(e.defaultSampleFlags,4),t}function uc(e){const t=e.flags&1?4:0,s=e.flags&4?4:0;let r=0;e.flags&256&&(r+=4),e.flags&512&&(r+=4),e.flags&1024&&(r+=4),e.flags&2048&&(r+=4);const i=r*e.sampleCount,n=new T("trun",16+t+s+i);n.writeFullBox(e.version,e.flags),n.writeUint(e.sampleCount,4),e.flags&1&&n.writeUint(e.dataOffset??0,4),e.flags&4&&n.writeUint(e.firstSampleFlags??0,4);for(const a of e.samples)e.flags&256&&n.writeUint(a.sampleDuration??0,4),e.flags&512&&n.writeUint(a.sampleSize??0,4),e.flags&1024&&n.writeUint(a.sampleFlags??0,4),e.flags&2048&&n.writeUint(a.sampleCompositionTimeOffset??0,4);return n}function dc(e){const t=e.location.length+1,s=new T("url ",12+t);return s.writeFullBox(e.version,e.flags),s.writeTerminatedString(e.location),s}function lc(e){const t=new T("vmhd",20);return t.writeFullBox(e.version,e.flags),t.writeUint(e.graphicsmode,2),t.writeArray(e.opcolor,W,2,3),t}const ws={mfhd:Bo,tfhd:Jo,tfdt:Go,trun:Wo,mdat:Zo};function se(e,t){for(const s of e){if(t(s))return s;const r=s.boxes;if(r&&Array.isArray(r)){const i=se(r,t);if(i)return i}}}function bs(e){const t=new ArrayBuffer(e.byteLength);return new Uint8Array(t).set(e),t}function de(e){return t=>t.type===e}function vs(e,t){const s=ms(bs(e),{readers:ws});return(se(s,de("tfdt"))?.baseMediaDecodeTime??0)*1e6/t}function gs(e,t){const s=ms(bs(e),{readers:ws}),r=se(s,de("tfdt"))?.baseMediaDecodeTime??0,i=se(s,de("tfhd")),n=i?.defaultSampleDuration??0,a=i?.defaultSampleSize??0,o=i?.defaultSampleFlags??0,c=se(s,de("trun"));if(!c)throw new Error("No trun box found in data segment");const u=se(s,de("mdat"));if(!u)throw new Error("No mdat box found in data segment");const d=u.data;if(!d)throw new Error("No data in mdat box");const f=[];let h=0,b=r;for(let v=0;v<c.sampleCount;v++){const U=c.samples[v]??{},q=U.sampleSize??a,D=U.sampleDuration??n;if(q<=0)throw new Error(`Invalid sample size ${q} for sample ${v} in trun`);if(D<=0)throw new Error(`Invalid sample duration ${D} for sample ${v} in trun`);if(h+q>d.length)throw new Error(`Sample ${v} would overflow mdat: offset=${h}, size=${q}, mdatLength=${d.length}`);const O=v===0&&c.firstSampleFlags!==void 0?c.firstSampleFlags:U.sampleFlags??o,y=U.sampleCompositionTimeOffset??0,x=new Uint8Array(d.slice(h,h+q));h+=q;const C=b+y,k=Math.round(C*1e6/t),V=O===0||(O&65536)===0;f.push({data:x,timestamp:k,keyframe:V}),b+=D}return f}function j(e){if(e=e.startsWith("0x")?e.slice(2):e,e.length%2)throw new Error("invalid hex string length");const t=e.match(/.{2}/g);if(!t)throw new Error("invalid hex string format");return new Uint8Array(t.map(s=>parseInt(s,16)))}const Ie=[65536,0,0,0,65536,0,0,0,1073741824],fc={ftyp:Xo,mvhd:tc,tkhd:oc,mdhd:Qo,hdlr:Yo,vmhd:lc,smhd:sc,"url ":dc,dref:Ho,stsd:rc,stts:ic,trex:cc,mfhd:ec,tfhd:ac,tfdt:nc,trun:uc,mdat:Ko};function fe(e){return Lo(e,{writers:fc})}function Be(e,t,s,r){const i=12+r.length,n=new Uint8Array(i),a=new DataView(n.buffer);return a.setUint32(0,i,!1),n[4]=e.charCodeAt(0),n[5]=e.charCodeAt(1),n[6]=e.charCodeAt(2),n[7]=e.charCodeAt(3),a.setUint32(8,t<<24|s,!1),n.set(r,12),n}function ys(){const e=new Uint8Array(4);return Be("stsc",0,0,e)}function _s(){const e=new Uint8Array(8);return Be("stsz",0,0,e)}function ks(){const e=new Uint8Array(4);return Be("stco",0,0,e)}function hc(e,t,s){const r=8+s.length,i=8+(78+r),n=new Uint8Array(i),a=new DataView(n.buffer);let o=0;return a.setUint32(o,i,!1),o+=4,n[o++]=97,n[o++]=118,n[o++]=99,n[o++]=49,o+=6,a.setUint16(o,1,!1),o+=2,a.setUint16(o,0,!1),o+=2,a.setUint16(o,0,!1),o+=2,o+=12,a.setUint16(o,e,!1),o+=2,a.setUint16(o,t,!1),o+=2,a.setUint32(o,4718592,!1),o+=4,a.setUint32(o,4718592,!1),o+=4,a.setUint32(o,0,!1),o+=4,a.setUint16(o,1,!1),o+=2,o+=32,a.setUint16(o,24,!1),o+=2,a.setUint16(o,65535,!1),o+=2,a.setUint32(o,r,!1),o+=4,n[o++]=97,n[o++]=118,n[o++]=99,n[o++]=67,n.set(s,o),n}function Et(e){const{codedWidth:t,codedHeight:s,description:r,container:i}=e;if(!t||!s||!r)throw new Error("Missing required fields to create video init segment");const n=i.kind==="cmaf"?i.timescale:1e6,a=i.kind==="cmaf"?i.trackId:1,o={type:"ftyp",majorBrand:"isom",minorVersion:512,compatibleBrands:["isom","iso6","mp41"]},c={type:"mvhd",version:0,flags:0,creationTime:0,modificationTime:0,timescale:n,duration:0,rate:65536,volume:256,reserved1:0,reserved2:[0,0],matrix:Ie,preDefined:[0,0,0,0,0,0],nextTrackId:a+1},u={type:"tkhd",version:0,flags:3,creationTime:0,modificationTime:0,trackId:a,reserved1:0,duration:0,reserved2:[0,0],layer:0,alternateGroup:0,volume:0,reserved3:0,matrix:Ie,width:t*65536,height:s*65536},d={type:"mdhd",version:0,flags:0,creationTime:0,modificationTime:0,timescale:n,duration:0,language:"und",preDefined:0},f={type:"hdlr",version:0,flags:0,preDefined:0,handlerType:"vide",reserved:[0,0,0],name:"VideoHandler"},h={type:"vmhd",version:0,flags:1,graphicsmode:0,opcolor:[0,0,0]},b={type:"dinf",boxes:[{type:"dref",version:0,flags:0,entryCount:1,entries:[{type:"url ",version:0,flags:1,location:""}]}]},v={type:"stsd",version:0,flags:0,entryCount:1,entries:[hc(t,s,j(r))]},U={type:"stts",version:0,flags:0,entryCount:0,entries:[]},q=ys(),D=_s(),O=ks(),y=fe([o,{type:"moov",boxes:[c,{type:"trak",boxes:[u,{type:"mdia",boxes:[d,f,{type:"minf",boxes:[h,b,{type:"stbl",boxes:[v,U,q,D,O]}]}]}]},{type:"mvex",boxes:[{type:"trex",version:0,flags:0,trackId:a,defaultSampleDescriptionIndex:1,defaultSampleDuration:0,defaultSampleSize:0,defaultSampleFlags:0}]}]}]),x=y.reduce((V,L)=>V+L.byteLength,0),C=new Uint8Array(x);let k=0;for(const V of y)C.set(new Uint8Array(V.buffer,V.byteOffset,V.byteLength),k),k+=V.byteLength;return C}function xt(e){const{sampleRate:t,numberOfChannels:s,description:r,codec:i,container:n}=e,a=n.kind==="cmaf"?n.timescale:1e6,o=n.kind==="cmaf"?n.trackId:1,c={type:"ftyp",majorBrand:"isom",minorVersion:512,compatibleBrands:["isom","iso6","mp41"]},u={type:"mvhd",version:0,flags:0,creationTime:0,modificationTime:0,timescale:a,duration:0,rate:65536,volume:256,reserved1:0,reserved2:[0,0],matrix:Ie,preDefined:[0,0,0,0,0,0],nextTrackId:o+1},d={type:"tkhd",version:0,flags:3,creationTime:0,modificationTime:0,trackId:o,reserved1:0,duration:0,reserved2:[0,0],layer:0,alternateGroup:0,volume:256,reserved3:0,matrix:Ie,width:0,height:0},f={type:"mdhd",version:0,flags:0,creationTime:0,modificationTime:0,timescale:a,duration:0,language:"und",preDefined:0},h={type:"hdlr",version:0,flags:0,preDefined:0,handlerType:"soun",reserved:[0,0,0],name:"SoundHandler"},b={type:"smhd",version:0,flags:0,balance:0,reserved:0},v={type:"dinf",boxes:[{type:"dref",version:0,flags:0,entryCount:1,entries:[{type:"url ",version:0,flags:1,location:""}]}]},U={type:"stsd",version:0,flags:0,entryCount:1,entries:[pc(i,t,s,r)]},q={type:"stts",version:0,flags:0,entryCount:0,entries:[]},D=ys(),O=_s(),y=ks(),x=fe([c,{type:"moov",boxes:[u,{type:"trak",boxes:[d,{type:"mdia",boxes:[f,h,{type:"minf",boxes:[b,v,{type:"stbl",boxes:[U,q,D,O,y]}]}]}]},{type:"mvex",boxes:[{type:"trex",version:0,flags:0,trackId:o,defaultSampleDescriptionIndex:1,defaultSampleDuration:0,defaultSampleSize:0,defaultSampleFlags:0}]}]}]),C=x.reduce((L,xs)=>L+xs.byteLength,0),k=new Uint8Array(C);let V=0;for(const L of x)k.set(new Uint8Array(L.buffer,L.byteOffset,L.byteLength),V),V+=L.byteLength;return k}function pc(e,t,s,r){if(e.startsWith("mp4a"))return mc(t,s,r);if(e==="opus")return wc(t,s,r);throw new Error(`Unsupported audio codec: ${e}`)}function mc(e,t,s){const r=vc(e,t,s),i=8+(28+r.length),n=new Uint8Array(i),a=new DataView(n.buffer);let o=0;return a.setUint32(o,i,!1),o+=4,n[o++]=109,n[o++]=112,n[o++]=52,n[o++]=97,o+=6,a.setUint16(o,1,!1),o+=2,o+=8,a.setUint16(o,t,!1),o+=2,a.setUint16(o,16,!1),o+=2,a.setUint16(o,0,!1),o+=2,a.setUint16(o,0,!1),o+=2,a.setUint32(o,e*65536,!1),o+=4,n.set(r,o),n}function wc(e,t,s){const r=gc(t,e,s),i=8+(28+r.length),n=new Uint8Array(i),a=new DataView(n.buffer);let o=0;return a.setUint32(o,i,!1),o+=4,n[o++]=79,n[o++]=112,n[o++]=117,n[o++]=115,o+=6,a.setUint16(o,1,!1),o+=2,o+=8,a.setUint16(o,t,!1),o+=2,a.setUint16(o,16,!1),o+=2,a.setUint16(o,0,!1),o+=2,a.setUint16(o,0,!1),o+=2,a.setUint32(o,e*65536,!1),o+=4,n.set(r,o),n}function bc(e,t){const s={96e3:0,88200:1,64e3:2,48e3:3,44100:4,32e3:5,24e3:6,22050:7,16e3:8,12e3:9,11025:10,8e3:11,7350:12}[e]??4,r=16|s>>1,i=(s&1)<<7|t<<3;return new Uint8Array([r,i])}function vc(e,t,s){const r=s?j(s):bc(e,t),i=r.length,n=15+i,a=5+n+3,o=14+a,c=new Uint8Array(o),u=new DataView(c.buffer);let d=0;return u.setUint32(d,o,!1),d+=4,c[d++]=101,c[d++]=115,c[d++]=100,c[d++]=115,u.setUint32(d,0,!1),d+=4,c[d++]=3,c[d++]=a,u.setUint16(d,0,!1),d+=2,c[d++]=0,c[d++]=4,c[d++]=n,c[d++]=64,c[d++]=21,c[d++]=0,c[d++]=0,c[d++]=0,u.setUint32(d,0,!1),d+=4,u.setUint32(d,0,!1),d+=4,c[d++]=5,c[d++]=i,c.set(r,d),d+=i,c[d++]=6,c[d++]=1,c[d++]=2,c}function gc(e,t,s){if(s){const o=j(s),c=8+o.length,u=new Uint8Array(c);return new DataView(u.buffer).setUint32(0,c,!1),u[4]=100,u[5]=79,u[6]=112,u[7]=115,u.set(o,8),u}const r=19,i=new Uint8Array(r),n=new DataView(i.buffer);let a=0;return n.setUint32(a,r,!1),a+=4,i[a++]=100,i[a++]=79,i[a++]=112,i[a++]=115,i[a++]=0,i[a++]=e,n.setUint16(a,312,!1),a+=2,n.setUint32(a,t,!1),a+=4,n.setInt16(a,0,!1),a+=2,i[a++]=0,i}function Is(e){const{data:t,timestamp:s,duration:r,keyframe:i,sequence:n,trackId:a=1}=e,o=i?33554432:16842752,c={type:"mfhd",version:0,flags:0,sequenceNumber:n},u={type:"tfhd",version:0,flags:131072,trackId:a},d={type:"tfdt",version:1,flags:0,baseMediaDecodeTime:s},f={type:"trun",version:0,flags:1793,sampleCount:1,dataOffset:0,samples:[{sampleDuration:r,sampleSize:t.byteLength,sampleFlags:o}]},h={type:"moof",boxes:[c,{type:"traf",boxes:[u,d,f]}]},b=fe([h]);let v=0;for(const k of b)v+=k.byteLength;f.dataOffset=v+8;const U=fe([h]);v=0;for(const k of U)v+=k.byteLength;const q=new ArrayBuffer(t.byteLength),D=new Uint8Array(q);D.set(t);const O=fe([{type:"mdat",data:D}]);let y=0;for(const k of O)y+=k.byteLength;const x=new Uint8Array(v+y);let C=0;for(const k of U)x.set(new Uint8Array(k.buffer,k.byteOffset,k.byteLength),C),C+=k.byteLength;for(const k of O)x.set(new Uint8Array(k.buffer,k.byteOffset,k.byteLength),C),C+=k.byteLength;return x}class ce{#t;#r;#e=[];#s;#i;#n=new p([]);buffered=this.#n;#a=new R;constructor(t,s){this.#t=t,this.#r=p.from(s?.latency??w.zero),this.#a.spawn(this.#o.bind(this)),this.#a.cleanup(()=>{this.#t.close();for(const r of this.#e)r.consumer.close();this.#e.length=0})}async#o(){for(;;){const t=await this.#t.nextGroup();if(!t)break;if(this.#s===void 0&&(this.#s=t.sequence),t.sequence<this.#s){console.warn(`skipping old group: ${t.sequence} < ${this.#s}`),t.close();continue}const s={consumer:t,frames:[]};this.#e.push(s),this.#e.sort((r,i)=>r.consumer.sequence-i.consumer.sequence),this.#a.spawn(this.#c.bind(this,s))}}async#c(t){try{let s=!0;for(;;){const r=await t.consumer.readFrame();if(!r)break;const{data:i,timestamp:n}=ce.#l(r),a={data:i,timestamp:n,keyframe:s};s=!1,t.frames.push(a),(t.latest===void 0||n>t.latest)&&(t.latest=n),this.#u(),t.consumer.sequence===this.#s?(this.#i?.(),this.#i=void 0):this.#d()}}catch{}finally{t.done=!0,t.consumer.sequence===this.#s&&(this.#s+=1),this.#u(),this.#i?.(),this.#i=void 0,t.consumer.close()}}#d(){if(this.#s===void 0)return;let t=!1;for(;this.#e.length>=2;){const s=Me.fromMilli(this.#r.peek());let r,i;for(const a of this.#e){if(a.latest===void 0)continue;const o=a.frames.at(0)?.timestamp??a.latest;(r===void 0||o<r)&&(r=o),(i===void 0||a.latest>i)&&(i=a.latest)}if(r===void 0||i===void 0||i-r<=s)break;const n=this.#e.shift();if(!n)break;this.#s=this.#e[0]?.consumer.sequence,console.warn(`skipping slow group: ${n.consumer.sequence} -> ${this.#s}`),n.consumer.close(),n.frames.length=0,t=!0}t&&(this.#u(),this.#i?.(),this.#i=void 0)}async next(){for(;;){if(this.#e.length>0&&this.#s!==void 0&&this.#e[0].consumer.sequence<=this.#s){const s=this.#e[0].frames.shift();if(s)return this.#u(),{frame:s,group:this.#e[0].consumer.sequence};if(this.#s>this.#e[0].consumer.sequence||this.#e[0].done){this.#e[0].consumer.sequence===this.#s&&(this.#s+=1);const r=this.#e.shift();if(r)return this.#u(),{frame:void 0,group:r.consumer.sequence}}}if(this.#i)throw new Error("multiple calls to decode not supported");const t=new Promise(s=>{this.#i=s}).then(()=>!0);if(!await Promise.race([t,this.#a.closed])){this.#i=void 0;return}}}static#l(t){const[s,r]=Os(t);return{timestamp:s,data:r}}#u(){const t=[];let s;for(const r of this.#e){const i=r.frames.at(0);if(!i||r.latest===void 0)continue;const n=w.fromMicro(i.timestamp),a=w.fromMicro(r.latest),o=t.at(-1),c=s?.done&&s.consumer.sequence+1===r.consumer.sequence;o&&(o.end>=n||c)?o.end=w.max(o.end,a):t.push({start:n,end:a}),s=r}this.#n.set(t)}close(){this.#a.close();for(const t of this.#e)t.consumer.close(),t.frames.length=0;this.#e.length=0}}navigator.userAgent.toLowerCase().includes("chrome");navigator.userAgent.toLowerCase().includes("firefox");let Te;async function At(){return globalThis.AudioEncoder&&globalThis.AudioDecoder?!0:(Te||(console.warn("using Opus polyfill; performance may be degraded"),Te=Promise.all([Je(()=>import("./libav-opus-af-BlMWboA7-CFTeN5TA.js"),[]),Je(()=>import("./main-DGBFe0O7-CIZu5tmC.js"),[])]).then(async([e,t])=>(await t.load({LibAV:e,polyfill:!0}),!0))),await Te)}const yc=`var __defProp = Object.defineProperty;
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

// src/audio/ring-buffer.ts
var AudioRingBuffer = class {
  #buffer;
  #writeIndex = 0;
  #readIndex = 0;
  rate;
  channels;
  #stalled = true;
  constructor(props) {
    if (props.channels <= 0) throw new Error("invalid channels");
    if (props.rate <= 0) throw new Error("invalid sample rate");
    if (props.latency <= 0) throw new Error("invalid latency");
    const samples = Math.ceil(props.rate * time_exports.Second.fromMilli(props.latency));
    if (samples === 0) throw new Error("empty buffer");
    this.rate = props.rate;
    this.channels = props.channels;
    this.#buffer = [];
    for (let i = 0; i < this.channels; i++) {
      this.#buffer[i] = new Float32Array(samples);
    }
  }
  get stalled() {
    return this.#stalled;
  }
  get timestamp() {
    return time_exports.Micro.fromSecond(this.#readIndex / this.rate);
  }
  get length() {
    return this.#writeIndex - this.#readIndex;
  }
  get capacity() {
    return this.#buffer[0]?.length;
  }
  resize(latency) {
    const newCapacity = Math.ceil(this.rate * time_exports.Second.fromMilli(latency));
    if (newCapacity === this.capacity) return;
    if (newCapacity === 0) throw new Error("empty buffer");
    const newBuffer = [];
    for (let i = 0; i < this.channels; i++) {
      newBuffer[i] = new Float32Array(newCapacity);
    }
    const samplesToKeep = Math.min(this.length, newCapacity);
    if (samplesToKeep > 0) {
      const copyStart = this.#writeIndex - samplesToKeep;
      for (let channel = 0; channel < this.channels; channel++) {
        const src = this.#buffer[channel];
        const dst = newBuffer[channel];
        for (let i = 0; i < samplesToKeep; i++) {
          const srcPos = (copyStart + i) % src.length;
          const dstPos = i % dst.length;
          dst[dstPos] = src[srcPos];
        }
      }
    }
    this.#buffer = newBuffer;
    this.#readIndex = this.#writeIndex - samplesToKeep;
    this.#stalled = true;
  }
  write(timestamp, data) {
    if (data.length !== this.channels) throw new Error("wrong number of channels");
    let start = Math.round(time_exports.Second.fromMicro(timestamp) * this.rate);
    let samples = data[0].length;
    let offset = this.#readIndex - start;
    if (offset > samples) {
      return;
    } else if (offset > 0) {
      samples -= offset;
      start += offset;
    } else {
      offset = 0;
    }
    const end = start + samples;
    const overflow = end - this.#readIndex - this.#buffer[0].length;
    if (overflow >= 0) {
      this.#stalled = false;
      this.#readIndex += overflow;
    }
    if (start > this.#writeIndex) {
      const gapSize = Math.min(start - this.#writeIndex, this.#buffer[0].length);
      if (gapSize === 1) {
        console.warn("floating point inaccuracy detected");
      }
      for (let channel = 0; channel < this.channels; channel++) {
        const dst = this.#buffer[channel];
        for (let i = 0; i < gapSize; i++) {
          const writePos = (this.#writeIndex + i) % dst.length;
          dst[writePos] = 0;
        }
      }
    }
    for (let channel = 0; channel < this.channels; channel++) {
      let src = data[channel];
      src = src.subarray(src.length - samples);
      const dst = this.#buffer[channel];
      if (src.length !== samples) throw new Error("mismatching number of samples");
      for (let i = 0; i < samples; i++) {
        const writePos = (start + i) % dst.length;
        dst[writePos] = src[i];
      }
    }
    if (end > this.#writeIndex) {
      this.#writeIndex = end;
    }
  }
  read(output) {
    if (output.length !== this.channels) throw new Error("wrong number of channels");
    if (this.#stalled) return 0;
    const samples = Math.min(this.#writeIndex - this.#readIndex, output[0].length);
    if (samples === 0) return 0;
    for (let channel = 0; channel < this.channels; channel++) {
      const dst = output[channel];
      const src = this.#buffer[channel];
      if (dst.length !== output[0].length) throw new Error("mismatching number of samples");
      for (let i = 0; i < samples; i++) {
        const readPos = (this.#readIndex + i) % src.length;
        dst[i] = src[readPos];
      }
    }
    this.#readIndex += samples;
    return samples;
  }
};

// src/audio/render-worklet.ts
var Render = class extends AudioWorkletProcessor {
  #buffer;
  #underflow = 0;
  #stateCounter = 0;
  constructor() {
    super();
    this.port.onmessage = (event) => {
      const { type } = event.data;
      if (type === "init") {
        this.#buffer = new AudioRingBuffer(event.data);
        this.#underflow = 0;
      } else if (type === "data") {
        if (!this.#buffer) throw new Error("buffer not initialized");
        this.#buffer.write(event.data.timestamp, event.data.data);
      } else if (type === "latency") {
        if (!this.#buffer) throw new Error("buffer not initialized");
        this.#buffer.resize(event.data.latency);
      } else {
        const exhaustive = type;
        throw new Error(\`unknown message type: \${exhaustive}\`);
      }
    };
  }
  process(_inputs, outputs, _parameters) {
    const output = outputs[0];
    const samplesRead = this.#buffer?.read(output) ?? 0;
    if (samplesRead < output[0].length) {
      this.#underflow += output[0].length - samplesRead;
    } else if (this.#underflow > 0 && this.#buffer) {
      console.warn(\`audio underflow: \${Math.round(1e3 * this.#underflow / this.#buffer.rate)}ms\`);
      this.#underflow = 0;
    }
    this.#stateCounter++;
    if (this.#buffer && this.#stateCounter >= 5) {
      this.#stateCounter = 0;
      const state = {
        type: "state",
        timestamp: this.#buffer.timestamp,
        stalled: this.#buffer.stalled
      };
      this.port.postMessage(state);
    }
    return true;
  }
};
registerProcessor("render", Render);
`,_c=new Blob([yc],{type:"application/javascript"}),kc=URL.createObjectURL(_c);let Ic=class{source;enabled;#t=new p(void 0);context=this.#t;#r=new p(void 0);root=this.#r;#e=new p(void 0);sampleRate=this.#e;#s=new p(void 0);stats=this.#s;#i=new p(void 0);timestamp=this.#i;#n=new p(!0);stalled=this.#n;#a=new p([]);#o=new p([]);buffered=this.#o;#c=new R;constructor(e,t){this.source=e,this.source.supported.set(Sc),this.enabled=p.from(t?.enabled??!1),this.#c.run(this.#d.bind(this)),this.#c.run(this.#l.bind(this)),this.#c.run(this.#u.bind(this)),this.#c.run(this.#h.bind(this))}#d(e){const t=e.get(this.source.config);if(!t)return;const s=t.sampleRate,r=t.numberOfChannels,i=new AudioContext({latencyHint:"interactive",sampleRate:s});e.set(this.#t,i),e.cleanup(()=>i.close()),e.spawn(async()=>{if(await i.audioWorklet.addModule(kc),i.state==="closed")return;const n=new AudioWorkletNode(i,"render",{channelCount:r,channelCountMode:"explicit"});e.cleanup(()=>n.disconnect());const a={type:"init",rate:s,channels:r,latency:this.source.sync.latency.peek()};n.port.postMessage(a),n.port.onmessage=o=>{if(o.data.type==="state"){const c=w.fromMicro(o.data.timestamp);this.#i.set(c),this.#n.set(o.data.stalled),this.#b(c)}},e.set(this.#r,n)})}#l(e){const t=e.getAll([this.enabled,this.#t]);if(!t)return;const[s,r]=t;r.resume()}#u(e){const t=e.get(this.#r);if(!t)return;const s={type:"latency",latency:e.get(this.source.sync.latency)};t.port.postMessage(s)}#h(e){if(!e.get(this.enabled))return;const t=e.get(this.source.broadcast);if(!t)return;const s=e.get(this.source.track);if(!s)return;const r=e.get(this.source.config);if(!r)return;const i=e.get(t.active);if(!i)return;const n=i.subscribe(s,ae.audio);e.cleanup(()=>n.close()),r.container.kind==="cmaf"?this.#m(e,n,r):this.#p(e,n,r)}#p(e,t,s){const r=new ce(t,{latency:this.source.sync.latency});e.cleanup(()=>r.close()),e.run(i=>{const n=i.get(r.buffered),a=i.get(this.#a);this.#o.update(()=>zc(n,a))}),e.spawn(async()=>{if(!await At())return;let i=0;const n=new AudioDecoder({output:o=>{if(i++,i<=3){o.close();return}this.#f(o)},error:o=>console.error(o)});e.cleanup(()=>n.close());const a=s.description?j(s.description):void 0;for(n.configure({...s,description:a});;){const o=await r.next();if(!o)break;const{frame:c}=o;if(!c)continue;this.#s.update(d=>({bytesReceived:(d?.bytesReceived??0)+c.data.byteLength}));const u=new EncodedAudioChunk({type:c.keyframe?"key":"delta",data:c.data,timestamp:c.timestamp});n.decode(u)}})}#m(e,t,s){if(s.container.kind!=="cmaf")return;const{timescale:r}=s.container,i=s.description?j(s.description):void 0;e.run(n=>{const a=n.get(this.#a);this.#o.update(()=>a)}),e.spawn(async()=>{if(!await At())return;const n=new AudioDecoder({output:a=>this.#f(a),error:a=>console.error(a)});for(e.cleanup(()=>n.close()),n.configure({codec:s.codec,sampleRate:s.sampleRate,numberOfChannels:s.numberOfChannels,description:i});;){const a=await t.nextGroup();if(!a)break;e.spawn(async()=>{try{for(;;){const o=await a.readFrame();if(!o)break;const c=gs(o,r);for(const u of c){this.#s.update(f=>({bytesReceived:(f?.bytesReceived??0)+u.data.byteLength}));const d=new EncodedAudioChunk({type:u.keyframe?"key":"delta",data:u.data,timestamp:u.timestamp});n.decode(d)}}}finally{a.close()}})}})}#f(e){const t=e.timestamp,s=w.fromMicro(t),r=this.#r.peek();if(!r){e.close();return}const i=e.numberOfFrames/e.sampleRate*1e6,n=w.fromMicro(i),a=w.add(s,n);this.#w(s,a);const o=[];for(let u=0;u<e.numberOfChannels;u++){const d=new Float32Array(e.numberOfFrames);e.copyTo(d,{format:"f32-planar",planeIndex:u}),o.push(d)}const c={type:"data",data:o,timestamp:t};r.port.postMessage(c,c.data.map(u=>u.buffer)),e.close()}#w(e,t){e>t||this.#a.mutate(s=>{for(const r of s)if(e<=r.end+1&&t>=r.start){r.start=w.min(r.start,e),r.end=w.max(r.end,t);return}s.push({start:e,end:t}),s.sort((r,i)=>r.start-i.start)})}#b(e){this.#a.mutate(t=>{for(;t.length>0;){if(t[0].end>=e){t[0].start=w.max(t[0].start,e);break}t.shift()}})}close(){this.#c.close()}};async function Sc(e){const t=e.description?j(e.description):void 0;return(await AudioDecoder.isConfigSupported({...e,description:t})).supported??!1}function zc(e,t){if(e.length===0)return t;if(t.length===0)return e;const s=[],r=[...e,...t].sort((i,n)=>i.start-n.start);for(const i of r){const n=s.at(-1);n&&n.end>=i.start?n.end=w.max(n.end,i.end):s.push({...i})}return s}const Ot=.001,Ue=.2;class Ec{source;volume;muted;paused;#t=new R;#r=.5;#e=new p(void 0);constructor(t,s){this.source=t,this.volume=p.from(s?.volume??.5),this.muted=p.from(s?.muted??!1),this.paused=p.from(s?.paused??s?.muted??!1),this.#t.run(r=>{r.get(this.muted)?(this.#r=this.volume.peek()||.5,this.volume.set(0)):this.volume.set(this.#r)}),this.#t.run(r=>{const i=!r.get(this.paused)&&!r.get(this.muted);this.source.enabled.set(i)}),this.#t.run(r=>{const i=r.get(this.volume);this.muted.set(i===0)}),this.#t.run(r=>{const i=r.get(this.source.root);if(!i)return;const n=new GainNode(i.context,{gain:r.get(this.volume)});i.connect(n),r.set(this.#e,n),r.run(a=>{a.get(this.source.enabled)&&(n.connect(i.context.destination),a.cleanup(()=>n.disconnect()))})}),this.#t.run(r=>{const i=r.get(this.#e);if(!i)return;r.cleanup(()=>i.gain.cancelScheduledValues(i.context.currentTime));const n=r.get(this.volume);n<Ot?(i.gain.exponentialRampToValueAtTime(Ot,i.context.currentTime+Ue),i.gain.setValueAtTime(0,i.context.currentTime+Ue+.01)):i.gain.exponentialRampToValueAtTime(n,i.context.currentTime+Ue)})}close(){this.#t.close()}}class xc{element;paused;#t;#r=new p(void 0);mediaSource=this.#r;#e=new R;constructor(t,s){this.element=p.from(s?.element),this.paused=p.from(s?.paused??!1),this.#t=t,this.#e.run(this.#s.bind(this)),this.#e.run(this.#i.bind(this)),this.#e.run(this.#n.bind(this)),this.#e.run(this.#a.bind(this)),this.#e.run(this.#o.bind(this))}#s(t){const s=t.get(this.element);if(!s)return;const r=new MediaSource;s.src=URL.createObjectURL(r),t.cleanup(()=>URL.revokeObjectURL(s.src)),t.event(r,"sourceopen",()=>{t.set(this.#r,r)},{once:!0}),t.event(r,"error",i=>{console.error("[MSE] MediaSource error event:",i)})}#i(t){const s=t.get(this.element);if(!s||t.get(this.paused))return;const r=w.toSecond(t.get(this.#t.latency));t.interval(()=>{const i=s.buffered;if(i.length===0)return;const n=i.end(i.length-1)-r,a=n-s.currentTime;(a>.1||a<-.1)&&(console.warn("seeking",a>0?"forward":"backward",Math.abs(a).toFixed(3),"seconds"),s.currentTime=n)},100)}#n(t){const s=t.get(this.element);if(!s)return;const r=t.get(this.mediaSource);r&&t.interval(async()=>{for(const i of r.sourceBuffers){for(;i.updating;)await new Promise(n=>i.addEventListener("updateend",n,{once:!0}));s.currentTime>10&&i.remove(0,s.currentTime-10)}},1e3)}#a(t){const s=t.get(this.element);if(!s)return;const r=t.get(this.paused);r&&!s.paused?s.pause():!r&&s.paused&&s.play().catch(i=>{console.error("[MSE] MediaElement play error:",i)})}#o(t){const s=t.get(this.element);if(!s||t.get(this.paused))return;const r=t.get(this.#t.reference);if(r===void 0)return;const i=t.get(this.#t.latency),n=w.sub(w.sub(w.now(),r),i);s.currentTime=w.toSecond(n)}close(){this.#e.close()}}class Ss{#t=new p(void 0);reference=this.#t;jitter;audio;video;#r=new p(w.zero);latency=this.#r;#e;#s;signals=new R;constructor(t){this.jitter=p.from(t?.jitter??100),this.audio=p.from(t?.audio),this.video=p.from(t?.video),this.#e=new Promise(s=>{this.#s=s}),this.signals.run(this.#i.bind(this))}#i(t){const s=t.get(this.jitter),r=t.get(this.video)??w.zero,i=t.get(this.audio)??w.zero,n=w.add(w.max(r,i),s);this.#r.set(n),this.#s(),this.#e=new Promise(a=>{this.#s=a})}received(t){const s=w.sub(w.now(),t),r=this.#t.peek();r!==void 0&&s>=r||(this.#t.set(s),this.#s(),this.#e=new Promise(i=>{this.#s=i}))}async wait(t){if(this.#t.peek()===void 0)throw new Error("reference not set; call update() first");for(;;){const s=w.now(),r=w.sub(s,t),i=this.#t.peek();if(i===void 0)return;const n=w.add(w.sub(i,r),this.#r.peek());if(n<=0)return;const a=new Promise(o=>setTimeout(o,n)).then(()=>!0);if(await Promise.race([this.#e,a]))return}}close(){this.signals.close()}}const Ac=500,Oc=100;class Pc{enabled;source;#t=new p(void 0);#r=new p(void 0);frame=this.#r;#e=new p(void 0);timestamp=this.#e;#s=new p(void 0);display=this.#s;#i=new p(!1);stalled=this.#i;#n=new p(void 0);stats=this.#n;#a=new p([]);buffered=this.#a;#o=new R;constructor(t,s){this.enabled=p.from(s?.enabled??!1),this.source=t,this.source.supported.set(Nc),this.#o.run(this.#c.bind(this)),this.#o.run(this.#d.bind(this)),this.#o.run(this.#l.bind(this)),this.#o.run(this.#u.bind(this))}#c(t){const s=t.getAll([this.enabled,this.source.broadcast,this.source.track,this.source.config]);if(!s){this.#t.set(void 0);return}const[r,i,n,a]=s,o=t.get(i.active);if(!o)return;let c=new Tc({source:this.source,broadcast:o,track:n,config:a,stats:this.#n});t.cleanup(()=>c?.close()),t.run(u=>{if(!c)return;const d=u.get(this.#t);if(d){const f=u.get(c.timestamp),h=u.get(d.timestamp);if(!f||h&&h>f+Oc)return}this.#t.set(c),c=void 0,u.close()})}#d(t){const s=t.get(this.#t);if(!s){this.#a.set([]);return}t.cleanup(()=>s.close()),t.run(r=>{const i=r.get(s.frame);this.#r.update(n=>(n?.close(),i?.clone()))}),t.proxy(this.#e,s.timestamp),t.proxy(this.#a,s.buffered)}#l(t){const s=t.get(this.source.catalog);if(!s)return;const r=s.display;if(r){t.set(this.#s,{width:r.width,height:r.height});return}const i=t.get(this.frame);i&&t.set(this.#s,{width:i.displayWidth,height:i.displayHeight})}#u(t){if(t.get(this.enabled)){if(!t.get(this.frame)){this.#i.set(!0);return}this.#i.set(!1),t.timer(()=>{this.#i.set(!0)},Ac)}}close(){this.#r.update(t=>{t?.close()}),this.#o.close()}}class Tc{source;broadcast;track;config;stats;timestamp=new p(void 0);frame=new p(void 0);buffered=new p([]);#t=new p([]);signals=new R;constructor(t){const{codedWidth:s,codedHeight:r,...i}=t.config;this.source=t.source,this.broadcast=t.broadcast,this.track=t.track,this.config=i,this.stats=t.stats,this.signals.run(this.#r.bind(this))}#r(t){const s=this.broadcast.subscribe(this.track,ae.video);t.cleanup(()=>s.close());const r=new VideoDecoder({output:async i=>{try{const n=w.fromMicro(i.timestamp);if(n<(this.timestamp.peek()??0))return;this.frame.peek()===void 0&&this.frame.set(i.clone());const a=this.source.sync.wait(n).then(()=>!0);if(!await Promise.race([a,t.cancel])||n<(this.timestamp.peek()??0))return;this.timestamp.set(n),this.#n(n),this.frame.update(o=>(o?.close(),i.clone()))}finally{i.close()}},error:i=>{console.error(i),t.close()}});t.cleanup(()=>r.close()),this.config.container.kind==="cmaf"?this.#s(t,s,r):this.#e(t,s,r)}#e(t,s,r){const i=new ce(s,{latency:this.source.sync.latency});t.cleanup(()=>i.close()),t.run(a=>{const o=a.get(i.buffered),c=a.get(this.#t);this.buffered.update(()=>Uc(o,c))}),r.configure({...this.config,description:this.config.description?j(this.config.description):void 0,optimizeForLatency:this.config.optimizeForLatency??!0,flip:!1});let n;t.spawn(async()=>{for(;;){const a=await Promise.race([i.next(),t.cancel]);if(!a)break;const{frame:o,group:c}=a;if(!o){n&&(n.final=!0);continue}this.source.sync.received(w.fromMicro(o.timestamp));const u=new EncodedVideoChunk({type:o.keyframe?"key":"delta",data:o.data,timestamp:o.timestamp});if(this.stats.update(d=>({frameCount:(d?.frameCount??0)+1,bytesReceived:(d?.bytesReceived??0)+o.data.byteLength})),n?.group===c||n?.final&&n.group+1===c){const d=w.fromMicro(n.timestamp),f=w.fromMicro(o.timestamp);this.#i(d,f)}n={timestamp:o.timestamp,group:c,final:!1},r.decode(u)}})}#s(t,s,r){if(this.config.container.kind!=="cmaf")return;const{timescale:i}=this.config.container,n=this.config.description?j(this.config.description):void 0;r.configure({codec:this.config.codec,description:n,optimizeForLatency:this.config.optimizeForLatency??!0,flip:!1}),t.run(a=>{const o=a.get(this.#t);this.buffered.update(()=>o)}),t.spawn(async()=>{for(;;){const a=await Promise.race([s.nextGroup(),t.cancel]);if(!a)break;t.spawn(async()=>{let o;try{for(;;){const c=await Promise.race([a.readFrame(),t.cancel]);if(!c)break;const u=gs(c,i);for(const d of u){const f=new EncodedVideoChunk({type:d.keyframe?"key":"delta",data:d.data,timestamp:d.timestamp});if(this.source.sync.received(w.fromMicro(d.timestamp)),this.stats.update(h=>({frameCount:(h?.frameCount??0)+1,bytesReceived:(h?.bytesReceived??0)+d.data.byteLength})),o!==void 0){const h=w.fromMicro(o),b=w.fromMicro(d.timestamp);this.#i(h,b)}o=d.timestamp,r.decode(f)}}}finally{a.close()}})}})}#i(t,s){t>s||this.#t.mutate(r=>{for(const i of r)if(i.start<=s&&i.end>=t){i.start=w.min(i.start,t),i.end=w.max(i.end,s);return}r.push({start:t,end:s}),r.sort((i,n)=>i.start-n.start)})}#n(t){this.#t.mutate(s=>{for(;s.length>0;){if(s[0].end>=t){s[0].start=w.max(s[0].start,t);break}s.shift()}})}close(){this.signals.close(),this.frame.update(t=>{t?.close()})}}function Uc(e,t){if(e.length===0)return t;if(t.length===0)return e;const s=[],r=[...e,...t].sort((i,n)=>i.start-n.start);for(const i of r){const n=s.at(-1);n&&n.end>=i.start?n.end=w.max(n.end,i.end):s.push({...i})}return s}async function Nc(e){const t=e.description?j(e.description):void 0,{supported:s}=await VideoDecoder.isConfigSupported({codec:e.codec,description:t,optimizeForLatency:e.optimizeForLatency??!0});return s??!1}let Rc=class{muxer;source;#t=new p(void 0);stats=this.#t;#r=new p([]);buffered=this.#r;#e=new p(!1);stalled=this.#e;#s=new p(w.zero);timestamp=this.#s;signals=new R;constructor(e,t){this.muxer=e,this.source=t,this.source.supported.set(qc),this.signals.run(this.#i.bind(this)),this.signals.run(this.#c.bind(this)),this.signals.run(this.#d.bind(this))}#i(e){const t=e.get(this.muxer.element);if(!t)return;const s=e.get(this.muxer.mediaSource);if(!s)return;const r=e.get(this.source.broadcast);if(!r)return;const i=e.get(r.active);if(!i)return;const n=e.get(this.source.track);if(!n)return;const a=e.get(this.source.config);if(!a)return;const o=`video/mp4; codecs="${a.codec}"`,c=s.addSourceBuffer(o);e.cleanup(()=>{s.removeSourceBuffer(c),c.abort()}),e.event(c,"error",u=>{console.error("[MSE] SourceBuffer error:",u)}),e.event(c,"updateend",()=>{this.#r.set(zs(c.buffered))}),a.container.kind==="cmaf"?this.#a(e,i,n,a,c,t):this.#o(e,i,n,a,c,t)}async#n(e,t){for(;e.updating;)await new Promise(s=>e.addEventListener("updateend",s,{once:!0}));for(e.appendBuffer(t);e.updating;)await new Promise(s=>e.addEventListener("updateend",s,{once:!0}))}#a(e,t,s,r,i,n){if(r.container.kind!=="cmaf")throw new Error("unreachable");const a=t.subscribe(s,ae.video);e.cleanup(()=>a.close());const o=r.container.timescale;e.spawn(async()=>{const c=Et(r);for(await this.#n(i,c);;){const u=await a.readFrame();if(!u)return;const d=vs(u,o);this.source.sync.received(w.fromMicro(d)),await this.#n(i,u),n.buffered.length>0&&n.currentTime<n.buffered.start(0)&&(n.currentTime=n.buffered.start(0))}})}#o(e,t,s,r,i,n){const a=t.subscribe(s,ae.video);e.cleanup(()=>a.close());const o=new ce(a,{latency:this.source.sync.latency});e.cleanup(()=>o.close()),e.spawn(async()=>{const c=Et(r);await this.#n(i,c);let u=1,d,f;for(;;){const h=await o.next();if(!h)return;if(h.frame){f=h.frame,this.source.sync.received(w.fromMicro(f.timestamp));break}}for(;;){const h=await o.next();if(h&&!h.frame)continue;const b=h?.frame;b&&(d=Me.sub(b.timestamp,f.timestamp),this.source.sync.received(w.fromMicro(b.timestamp)));const v=Is({data:f.data,timestamp:f.timestamp,duration:d??0,keyframe:f.keyframe,sequence:u++});if(await this.#n(i,v),n.buffered.length>0&&n.currentTime<n.buffered.start(0)&&(n.currentTime=n.buffered.start(0)),!b)return;f=b}})}#c(e){const t=e.get(this.muxer.element);if(!t)return;const s=()=>{this.#e.set(t.readyState<=HTMLMediaElement.HAVE_CURRENT_DATA)};s(),e.event(t,"waiting",s),e.event(t,"playing",s),e.event(t,"seeking",s)}#d(e){const t=e.get(this.muxer.element);if(t)if("requestVideoFrameCallback"in t){const s=t;let r;const i=()=>{const n=w.fromSecond(s.currentTime);this.#s.set(n),r=s.requestVideoFrameCallback(i)};r=s.requestVideoFrameCallback(i),e.cleanup(()=>s.cancelVideoFrameCallback(r))}else e.event(t,"timeupdate",()=>{const s=w.fromSecond(t.currentTime);this.#s.set(s)})}close(){this.source.close(),this.signals.close()}};async function qc(e){return MediaSource.isTypeSupported(`video/mp4; codecs="${e.codec}"`)}class Mc{decoder;canvas;paused;#t;#r=new p(void 0);#e=new R;constructor(t,s){this.decoder=t,this.canvas=p.from(s?.canvas),this.paused=p.from(s?.paused??!1),this.#e.run(r=>{const i=r.get(this.canvas);this.#r.set(i?.getContext("2d")??void 0)}),this.#e.run(this.#i.bind(this)),this.#e.run(this.#n.bind(this)),this.#e.run(this.#s.bind(this))}#s(t){const s=t.getAll([this.canvas,this.decoder.display]);if(!s)return;const[r,i]=s;(r.width!==i.width||r.height!==i.height)&&(r.width=i.width,r.height=i.height)}#i(t){const s=t.get(this.canvas);if(!s||t.get(this.paused))return;let r=!1;const i=new IntersectionObserver(a=>{for(const o of a)r=o.isIntersecting,this.decoder.enabled.set(r&&!document.hidden)},{threshold:.01}),n=()=>{this.decoder.enabled.set(r&&!document.hidden)};document.addEventListener("visibilitychange",n),t.cleanup(()=>document.removeEventListener("visibilitychange",n)),t.cleanup(()=>this.decoder.enabled.set(!1)),i.observe(s),t.cleanup(()=>i.disconnect())}#n(t){const s=t.get(this.#r);if(!s)return;let r;t.get(this.paused)?r=this.#t?.clone():(r=t.get(this.decoder.frame),this.#t?.close(),this.#t=r?.clone());let i=requestAnimationFrame(()=>{this.#a(s,r),i=void 0});t.cleanup(()=>{r?.close(),i&&cancelAnimationFrame(i)})}#a(t,s){if(!s){t.fillStyle="#000",t.fillRect(0,0,t.canvas.width,t.canvas.height);return}t.save(),t.fillStyle="#000",t.fillRect(0,0,t.canvas.width,t.canvas.height),this.decoder.source.catalog.peek()?.flip&&(t.scale(-1,1),t.translate(-t.canvas.width,0)),t.drawImage(s,0,0,t.canvas.width,t.canvas.height),t.restore()}close(){this.#t?.close(),this.#t=void 0,this.#e.close()}}function $c(e){return t=>{const s=[],r=[];for(const[i,n]of t)if(n.codedWidth&&n.codedHeight){const a=n.codedWidth*n.codedHeight;a<=e?s.push({name:i,size:a}):r.push({name:i,size:a})}return s.sort((i,n)=>n.size-i.size),s.length>0?s.map(i=>i.name):r.length>0?(r.sort((i,n)=>i.size-n.size),[r[0].name]):t.map(([i])=>i)}}function Cc(e){return t=>{const s=[],r=[];for(const[i,n]of t)n.bitrate!=null&&n.bitrate<=e?s.push({name:i,bitrate:n.bitrate}):n.bitrate!=null&&r.push({name:i,bitrate:n.bitrate});return s.sort((i,n)=>n.bitrate-i.bitrate),s.length>0?s.map(i=>i.name):r.length>0?(r.sort((i,n)=>i.bitrate-n.bitrate),[r[0].name]):t.map(([i])=>i)}}function Dc(e){let t=e[0];for(const s of e){const[,r]=s,[,i]=t,n=(r.codedWidth??0)*(r.codedHeight??0),a=(i.codedWidth??0)*(i.codedHeight??0);if(n!==a){n>a&&(t=s);continue}(r.bitrate??0)>(i.bitrate??0)&&(t=s)}return t[0]}let Fc=class{broadcast;target;#t=new p(void 0);catalog=this.#t;#r=new p({});available=this.#r;#e=new p(void 0);track=this.#e;#s=new p(void 0);config=this.#s;sync;supported;#i=new R;constructor(e,t){this.broadcast=p.from(t?.broadcast),this.target=p.from(t?.target),this.sync=e,this.supported=p.from(t?.supported),this.#i.run(this.#n.bind(this)),this.#i.run(this.#a.bind(this)),this.#i.run(this.#o.bind(this))}#n(e){const t=e.get(this.broadcast);if(!t)return;const s=e.get(t.catalog)?.video;s&&e.set(this.#t,s)}#a(e){const t=e.get(this.supported);if(!t)return;const s=e.get(this.#t)?.renditions??{};e.spawn(async()=>{const r={};for(const[i,n]of Object.entries(s))await t(n)&&(r[i]=n);Object.keys(r).length===0&&Object.keys(s).length>0&&console.warn("[Source] No supported video renditions found:",s),this.#r.set(r)})}#o(e){const t=e.get(this.#r);if(Object.keys(t).length===0)return;const s=e.get(this.target),r=s?.name,i=r&&r in t?r:this.#c(t,s);if(!i)return;const n=t[i];e.set(this.#e,i),e.set(this.#s,n),e.set(this.sync.video,n.jitter)}#c(e,t){const s=Object.entries(e);if(s.length===0)return;if(s.length===1)return s[0][0];const r=[];if(t?.pixels!=null&&r.push($c(t.pixels)),t?.bitrate!=null&&r.push(Cc(t.bitrate)),r.length===0)return Dc(s);const i=r.map(a=>a(s)),n=i.map(a=>new Set(a));for(const a of i[0])if(n.every(o=>o.has(a)))return a;console.warn("conflicting rendition filters, no rendition satisfies all criteria")}close(){this.#i.close()}};function zs(e){const t=[];for(let s=0;s<e.length;s++){const r=w.fromSecond(e.start(s)),i=w.fromSecond(e.end(s));t.push({start:r,end:i})}return t}class Vc{source;stats=new p(void 0);stalled=new p(!1);buffered=new p([]);timestamp=new p(w.zero);constructor(t){this.source=t}}class jc{source;volume=new p(.5);muted=new p(!1);stats=new p(void 0);buffered=new p([]);constructor(t){this.source=t}}class Lc{element=new p(void 0);broadcast;jitter;paused;video;#t;audio;#r;#e;signals=new R;constructor(t){this.element=p.from(t?.element),this.broadcast=p.from(t?.broadcast),this.jitter=p.from(t?.jitter??100),this.#e=new Ss({jitter:this.jitter}),this.#t=new Fc(this.#e,{broadcast:this.broadcast}),this.#r=new Gc(this.#e,{broadcast:this.broadcast}),this.video=new Vc(this.#t),this.audio=new jc(this.#r),this.paused=p.from(t?.paused??!1),this.signals.run(this.#s.bind(this))}#s(t){const s=t.get(this.element);s&&(s instanceof HTMLCanvasElement?this.#i(t,s):s instanceof HTMLVideoElement&&this.#n(t,s))}#i(t,s){const r=new Pc(this.#t),i=new Ic(this.#r),n=new Ec(i,{volume:this.audio.volume,muted:this.audio.muted,paused:this.paused}),a=new Mc(r,{canvas:s,paused:this.paused});t.cleanup(()=>{r.close(),i.close(),n.close(),a.close()}),t.proxy(this.video.stats,r.stats),t.proxy(this.video.buffered,r.buffered),t.proxy(this.video.stalled,r.stalled),t.proxy(this.video.timestamp,r.timestamp),t.proxy(this.audio.stats,i.stats),t.proxy(this.audio.buffered,i.buffered)}#n(t,s){const r=new xc(this.#e,{paused:this.paused,element:s}),i=new Rc(r,this.#t),n=new Zc(r,this.#r,{volume:this.audio.volume,muted:this.audio.muted});t.cleanup(()=>{i.close(),n.close(),r.close()}),t.proxy(this.video.stats,i.stats),t.proxy(this.video.buffered,i.buffered),t.proxy(this.video.stalled,i.stalled),t.proxy(this.video.timestamp,i.timestamp),t.proxy(this.audio.stats,n.stats),t.proxy(this.audio.buffered,n.buffered)}close(){this.signals.close()}}class Zc{muxer;source;volume;muted;#t=new p(void 0);stats=this.#t;#r=new p([]);buffered=this.#r;#e=new R;constructor(t,s,r){this.muxer=t,this.source=s,this.source.supported.set(Bc),this.volume=p.from(r?.volume??.5),this.muted=p.from(r?.muted??!1),this.#e.run(this.#s.bind(this)),this.#e.run(this.#o.bind(this))}#s(t){const s=t.get(this.muxer.element);if(!s)return;const r=t.get(this.muxer.mediaSource);if(!r)return;const i=t.get(this.source.broadcast);if(!i)return;const n=t.get(i.active);if(!n)return;const a=t.get(this.source.track);if(!a)return;const o=t.get(this.source.config);if(!o)return;const c=`audio/mp4; codecs="${o.codec}"`,u=r.addSourceBuffer(c);t.cleanup(()=>{r.removeSourceBuffer(u),u.abort()}),t.event(u,"error",f=>{console.error("[MSE] SourceBuffer error:",f)}),t.event(u,"updateend",()=>{this.#r.set(zs(u.buffered))});const d=n.subscribe(a,ae.audio);t.cleanup(()=>d.close()),o.container.kind==="cmaf"?this.#n(t,d,o,u,s):this.#a(t,d,o,u,s)}async#i(t,s){for(;t.updating;)await new Promise(r=>t.addEventListener("updateend",r,{once:!0}));for(t.appendBuffer(s);t.updating;)await new Promise(r=>t.addEventListener("updateend",r,{once:!0}))}#n(t,s,r,i,n){if(r.container.kind!=="cmaf")throw new Error("unreachable");const a=r.container.timescale;t.spawn(async()=>{const o=xt(r);for(await this.#i(i,o);;){const c=await s.readFrame();if(!c)return;const u=vs(c,a);this.source.sync.received(w.fromMicro(u)),await this.#i(i,c),n.buffered.length>0&&n.currentTime<n.buffered.start(0)&&(n.currentTime=n.buffered.start(0))}})}#a(t,s,r,i,n){const a=new ce(s,{latency:this.source.sync.latency});t.cleanup(()=>a.close()),t.spawn(async()=>{const o=xt(r);await this.#i(i,o);let c=1,u,d;for(;;){const f=await a.next();if(!f)return;if(f.frame){d=f.frame,this.source.sync.received(w.fromMicro(d.timestamp));break}}for(;;){const f=await a.next();if(f&&!f.frame)continue;const h=f?.frame;h&&(u=Me.sub(h.timestamp,d.timestamp),this.source.sync.received(w.fromMicro(h.timestamp)));const b=Is({data:d.data,timestamp:d.timestamp,duration:u??0,keyframe:d.keyframe,sequence:c++});if(await this.#i(i,b),n.buffered.length>0&&n.currentTime<n.buffered.start(0)&&(n.currentTime=n.buffered.start(0)),!h)return;d=h}})}#o(t){const s=t.get(this.muxer.element);if(!s)return;const r=t.get(this.volume),i=t.get(this.muted);i&&!s.muted?s.muted=!0:!i&&s.muted&&(s.muted=!1),r!==s.volume&&(s.volume=r),t.event(s,"volumechange",()=>{this.volume.set(s.volume)})}close(){this.#e.close()}}async function Bc(e){return MediaSource.isTypeSupported(`audio/mp4; codecs="${e.codec}"`)}class Gc{broadcast;target;#t=new p(void 0);catalog=this.#t;#r=new p({});available=this.#r;#e=new p(void 0);track=this.#e;#s=new p(void 0);config=this.#s;supported;sync;#i=new R;constructor(t,s){this.sync=t,this.broadcast=p.from(s?.broadcast),this.target=p.from(s?.target),this.supported=p.from(s?.supported),this.#i.run(this.#n.bind(this)),this.#i.run(this.#a.bind(this)),this.#i.run(this.#o.bind(this))}#n(t){const s=t.get(this.broadcast);if(!s)return;const r=t.get(s.catalog)?.audio;r&&t.set(this.#t,r)}#a(t){const s=t.get(this.#t)?.renditions??{},r=t.get(this.supported);r&&t.spawn(async()=>{const i={};for(const[n,a]of Object.entries(s))await r(a)&&(i[n]=a);Object.keys(i).length===0&&Object.keys(s).length>0&&console.warn("no supported audio renditions found:",s),this.#r.set(i)})}#o(t){const s=t.get(this.#r);if(Object.keys(s).length===0)return;const r=t.get(this.target);let i;if(r?.name&&r.name in s)i={track:r.name,config:s[r.name]};else if(i=this.#c(s),!i)return;t.set(this.#e,i.track),t.set(this.#s,i.config),t.set(this.sync.audio,i.config.jitter)}#c(t){const s=Object.entries(t);if(s.length!==0){for(const[r,i]of s)if(i.container.kind==="legacy")return{track:r,config:i};for(const[r,i]of s)if(i.container.kind==="cmaf")return{track:r,config:i}}}close(){this.#i.close()}}class Jc{connection;enabled;name;status=new p("offline");reload;#t=new p(void 0);active=this.#t;#r=new p(void 0);catalog=this.#r;#e=new p(!1);signals=new R;constructor(t){this.connection=p.from(t?.connection),this.name=p.from(t?.name??As()),this.enabled=p.from(t?.enabled??!1),this.reload=p.from(t?.reload??!1),this.signals.run(this.#s.bind(this)),this.signals.run(this.#i.bind(this)),this.signals.run(this.#n.bind(this))}#s(t){if(!t.get(this.enabled))return;if(!t.get(this.reload)){t.set(this.#e,!0,!1);return}const s=t.get(this.connection);if(!s)return;const r=t.get(this.name),i=s.announced(r);t.cleanup(()=>i.close()),t.spawn(async()=>{for(;;){const n=await i.next();if(!n)break;if(n.path!==r){console.warn("ignoring announce",n.path);continue}t.set(this.#e,n.active,!1)}})}#i(t){const s=t.getAll([this.enabled,this.#e,this.connection]);if(!s)return;const[r,i,n]=s,a=t.get(this.name),o=n.consume(a);t.cleanup(()=>o.close()),t.set(this.#t,o)}#n(t){const s=t.getAll([this.enabled,this.active]);if(!s)return;const[r,i]=s;this.status.set("loading");const n=i.subscribe("catalog.json",ae.catalog);t.cleanup(()=>n.close()),t.spawn(this.#a.bind(this,n))}async#a(t){try{for(;;){const s=await Oo(t);if(!s)break;console.debug("received catalog",this.name.peek(),s),this.#r.set(s),this.status.set("live")}}catch(s){console.warn("error fetching catalog",this.name.peek(),s)}finally{this.#r.set(void 0),this.status.set("offline")}}close(){this.signals.close()}}const Wc=["url","name","paused","volume","muted","reload","jitter"],Hc=new FinalizationRegistry(e=>e.close());class Xc extends HTMLElement{static observedAttributes=Wc;connection;broadcast;sync=new Ss;backend;#t=new p(!1);signals=new R;constructor(){super(),Hc.register(this,this.signals),this.connection=new Ps({enabled:this.#t}),this.signals.cleanup(()=>this.connection.close()),this.broadcast=new Jc({connection:this.connection.established,enabled:this.#t}),this.signals.cleanup(()=>this.broadcast.close()),this.backend=new Lc({broadcast:this.broadcast}),this.signals.cleanup(()=>this.backend.close());const t=()=>{const r=this.querySelector("canvas"),i=this.querySelector("video");if(r&&i)throw new Error("Cannot have both canvas and video elements");this.backend.element.set(r??i)},s=new MutationObserver(t);s.observe(this,{childList:!0,subtree:!0}),this.signals.cleanup(()=>s.disconnect()),t(),this.signals.run(r=>{const i=r.get(this.connection.url);i?this.setAttribute("url",i.toString()):this.removeAttribute("url")}),this.signals.run(r=>{const i=r.get(this.broadcast.name);this.setAttribute("name",i.toString())}),this.signals.run(r=>{r.get(this.backend.audio.muted)?this.setAttribute("muted",""):this.removeAttribute("muted")}),this.signals.run(r=>{r.get(this.backend.paused)?this.setAttribute("paused","true"):this.removeAttribute("paused")}),this.signals.run(r=>{const i=r.get(this.backend.audio.volume);this.setAttribute("volume",i.toString())}),this.signals.run(r=>{const i=Math.floor(r.get(this.backend.jitter));this.setAttribute("jitter",i.toString())})}connectedCallback(){this.#t.set(!0),this.style.display="block",this.style.position="relative"}disconnectedCallback(){this.#t.set(!1)}attributeChangedCallback(t,s,r){if(s!==r)if(t==="url")this.connection.url.set(r?new URL(r):void 0);else if(t==="name")this.broadcast.name.set(We(r??""));else if(t==="paused")this.backend.paused.set(r!==null);else if(t==="volume"){const i=r?Number.parseFloat(r):.5;this.backend.audio.volume.set(i)}else if(t==="muted")this.backend.audio.muted.set(r!==null);else if(t==="reload")this.broadcast.reload.set(r!==null);else if(t==="jitter")this.backend.jitter.set(r?Number.parseFloat(r):100);else{const i=t;throw new Error(`Invalid attribute: ${i}`)}}get url(){return this.connection.url.peek()}set url(t){this.connection.url.set(t?new URL(t):void 0)}get name(){return this.broadcast.name.peek()}set name(t){this.broadcast.name.set(We(t))}get paused(){return this.backend.paused.peek()}set paused(t){this.backend.paused.set(t)}get volume(){return this.backend.audio.volume.peek()}set volume(t){this.backend.audio.volume.set(t)}get muted(){return this.backend.audio.muted.peek()}set muted(t){this.backend.audio.muted.set(t)}get reload(){return this.broadcast.reload.peek()}set reload(t){this.broadcast.reload.set(t)}get jitter(){return this.backend.jitter.peek()}set jitter(t){this.backend.jitter.set(t)}}customElements.define("moq-watch",Xc);const Ge=document.getElementById("watch");if(!Ge)throw new Error("missing <moq-watch> element");const Es=new URLSearchParams(window.location.search),Yc=Es.get("name")??"hello",Kc=Es.get("url")??`${window.location.origin}/`;Ge.url=Kc;Ge.name=Yc;
