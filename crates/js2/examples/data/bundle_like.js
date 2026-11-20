//= 125972
(function(modules){
var cache={};
function r(id){
var c=cache[id];
if(c!==undefined)return c.exports;
var m=cache[id]={exports:{}};
modules[id](m,m.exports,r);
return m.exports;
}
r.d=function(exports,def){
for(var key in def){
if(!r.o(def,key)||r.o(exports,key))continue;
Object.defineProperty(exports,key,{enumerable:true,get:def[key]});
}
};
r.o=function(o,k){return Object.prototype.hasOwnProperty.call(o,k)};
r.r=function(exports){Object.defineProperty(exports,"__esModule",{value:true});};
return r(0);
})({
0:function(e,t,n){
n.r(t);
var u=n(1),s=n(2),h=n(3);
var store=s.createStore(function(state,action){
switch(action.type){
case"add":return u.assign({},state,{items:state.items.concat([action.item]),count:state.count+1});
case"remove":return u.assign({},state,{items:state.items.filter(function(x){return x.id!==action.id}),count:state.count-1});
case"rename":return u.assign({},state,{items:state.items.map(function(x){return x.id===action.id?u.assign({},x,{name:action.name}):x})});
case"toggle":return u.assign({},state,{on:!state.on});
default:return state;
}
},{items:[],count:0,on:false});
var renders=0,total=0;
store.subscribe(function(){
renders++;
var tree=h.h("ul",{className:"list",id:"l"+renders},store.getState().items.map(function(it){
return h.h("li",{key:it.id,className:it.id%2?"odd":"even"},h.h("span",null,it.name),store.getState().on?h.h("b",null,"on"):null);
}));
total+=h.render(tree).length;
});
for(var i=0;i<60;i++){
store.dispatch({type:"add",item:{id:i,name:"item"+i}});
if(i%3==0)store.dispatch({type:"rename",id:i>>1,name:"n"+i});
if(i%5==0)store.dispatch({type:"toggle"});
if(i%7==0)store.dispatch({type:"remove",id:i>>2});
}
e.exports=String(total+renders+store.getState().count);
t.default=e.exports;
return e.exports;
},
1:function(e,t,n){
n.r(t);
function isFn(x){return typeof x==="function"}
function assign(target){
for(var i=1;i<arguments.length;i++){
var src=arguments[i];
if(src!==null&&src!==undefined){
for(var k in src){if(Object.prototype.hasOwnProperty.call(src,k))target[k]=src[k];}
}
}
return target;
}
n.d(t,{assign:function(){return assign},isFn:function(){return isFn}});
},
2:function(e,t,n){
n.r(t);
var u=n(1);
function createStore(reducer,init){
var state=init,listeners=[];
return{
getState:function(){return state},
subscribe:function(f){listeners.push(f);return function(){listeners=listeners.filter(function(g){return g!==f})}},
dispatch:function(a){
state=reducer(state,a);
for(var i=0;i<listeners.length;i++){var l=listeners[i];if(u.isFn(l))l();}
return a;
}
};
}
n.d(t,{createStore:function(){return createStore}});
},
3:function(e,t,n){
n.r(t);
function h(type,props){
var kids=[];
for(var i=2;i<arguments.length;i++){
var c=arguments[i];
if(c instanceof Array){for(var j=0;j<c.length;j++)kids.push(c[j]);}
else if(c!==null&&c!==undefined&&c!==false)kids.push(c);
}
return{type:type,props:props||{},children:kids};
}
function esc(s){
var o="";
for(var i=0;i<s.length;i++){var c=s.charAt(i);o+=c==="<"?"&lt;":c===">"?"&gt;":c;}
return o;
}
function render(node){
if(typeof node==="string")return esc(node);
var out="<"+node.type;
for(var k in node.props){
if(k!=="key")out+=" "+k+'="'+node.props[k]+'"';
}
out+=">";
for(var i=0;i<node.children.length;i++)out+=render(node.children[i]);
return out+"</"+node.type+">";
}
n.d(t,{h:function(){return h},render:function(){return render}});
}
})
