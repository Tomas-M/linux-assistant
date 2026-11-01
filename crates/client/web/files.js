'use strict';
const localFileRequests=new Map(),pendingLocalFiles=new Map();
let localFileTimer;

// Recognize local paths, including Markdown URLs and editor line references, without treating web URLs as files.
function localFilePath(value,encoded=false,relative=false)
{
    let path=value.trim();
    if (!path || new TextEncoder().encode(path).length>4096 || /[\u0000-\u001f\u007f]/u.test(path)) return null;
    if (/^file:/iu.test(path))
    {
        try
        {
            const url=new URL(path);
            if (url.hostname && url.hostname!=='localhost') return null;
            path=decodeURIComponent(url.pathname);
            if (/^\/[a-z]:\//iu.test(path)) path=path.slice(1);
        }
        catch { return null; }
    }
    else if (encoded)
    {
        try { path=decodeURIComponent(path); }
        catch { return null; }
    }
    if (/^(?:https?:|mailto:|#|\/\/|\\\\)/iu.test(path)) return null;
    path=path.replace(/(?:#L\d+(?:C\d+)?(?:-L?\d+(?:C\d+)?)?|:\d+(?::\d+)?)$/u,'');
    if (/^(?:\/(?!\/)|~[\/\\]|\.{1,2}[\/\\]|[a-z]:[\/\\])/iu.test(path)) return path;
    if (relative && !/[:<>|\r\n]/u.test(path) && /[\/\\.]\S/u.test(path)) return path;
    return null;
}

// Coalesce checks from streamed text; the native client reads only metadata until a link is clicked.
function resolveLocalFile(path)
{
    if (localFileRequests.has(path)) return localFileRequests.get(path);
    const result=new Promise(resolve=>pendingLocalFiles.set(path,resolve));
    localFileRequests.set(path,result);
    localFileTimer ??=setTimeout(resolveLocalFiles,100);
    return result;
}

// Small batches fit the local request budget even when paths contain international characters.
async function resolveLocalFiles()
{
    localFileTimer=undefined;
    const batch=[...pendingLocalFiles.entries()].slice(0,8);
    for (const [path] of batch) pendingLocalFiles.delete(path);
    let files=[];
    try { files=(await api('files',batch.map(([path])=>path))).files; }
    catch { /* A missing or unavailable file stays ordinary response text. */ }
    for (const [path,resolve] of batch)
    {
        const file=files.find(file=>file.path===path);
        if (!file) localFileRequests.delete(path);
        resolve(file);
    }
    if (pendingLocalFiles.size) localFileTimer ??=setTimeout(resolveLocalFiles,100);
}

// Preview through the local server and copy the actual disk path independently of its opaque URL.
function setLocalFileLink(link,file)
{
    link.href=file.url; link.target='_blank'; link.rel='noopener noreferrer'; link.classList.add('local-file-link');
    link.title='Open '+file.name;
    const wrapper=document.createElement('span'),copy=document.createElement('button');
    wrapper.className='local-file'; copy.type='button'; copy.className='icon-button copy-file-path';
    copy.dataset.copyFilePath=file.full_path; copy.title='Copy file path'; copy.setAttribute('aria-label','Copy file path');
    setIcon(copy,'copy'); link.replaceWith(wrapper); wrapper.append(link,copy);
    return wrapper;
}

// Preserve inline code and formatting, and ignore paths inside commands or fenced code blocks.
function linkLocalFiles(element)
{
    for (const link of element.querySelectorAll('a[data-local-file]'))
    {
        const path=link.dataset.localFile; link.removeAttribute('href');
        resolveLocalFile(path).then(file=>
        {
            if (!element.contains(link)) return;
            if (file) setLocalFileLink(link,file);
            else link.replaceWith(...link.childNodes);
        });
    }
    const walker=document.createTreeWalker(element,NodeFilter.SHOW_TEXT),nodes=[];
    while (walker.nextNode()) if (!walker.currentNode.parentElement.closest('a,pre')) nodes.push(walker.currentNode);
    for (const node of nodes)
    {
        const text=node.textContent,code=node.parentElement.closest('code'),matches=[];
        const whole=code && localFilePath(text,false,true);
        if (whole) matches.push({start:0,end:text.length,path:whole});
        else
        {
            const pattern=/"([^"\n]+)"|'([^'\n]+)'|“([^”\n]+)”|‘([^’\n]+)’|(?:[a-z]:[\\/]|~\/|\.{1,2}\/|\/)[^\s<>"'`|]+/giu;
            for (const match of text.matchAll(pattern))
            {
                if (match.index && !/[\s([{:'"“‘]/u.test(text[match.index-1])) continue;
                const quoted=match[1] ?? match[2] ?? match[3] ?? match[4];
                const value=quoted ?? match[0].replace(/[.,;!?)\]}]+$/u,'');
                const path=localFilePath(value,false,quoted!==undefined),start=match.index+(quoted===undefined?0:1);
                if (path) matches.push({start,end:start+value.length,path});
            }
        }
        if (!matches.length) continue;
        Promise.all(matches.map(match=>resolveLocalFile(match.path))).then(files=>
        {
            if (!element.contains(node) || !files.some(Boolean)) return;
            const fragment=document.createDocumentFragment(); let offset=0;
            matches.forEach((match,index)=>
            {
                if (!files[index]) return;
                fragment.append(text.slice(offset,match.start));
                const link=document.createElement('a'); link.textContent=text.slice(match.start,match.end);
                fragment.append(setLocalFileLink(link,files[index])); offset=match.end;
            });
            fragment.append(text.slice(offset)); node.replaceWith(fragment);
        });
    }
}
