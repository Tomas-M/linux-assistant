'use strict';
let draftAttachments=[], attaching=false, attachmentError='';

// FileReader preserves the original encoded bytes without re-encoding the image.
function imageData(file)
{
    return new Promise((resolve,reject)=>
    {
        const reader=new FileReader(); reader.onload=()=>resolve(reader.result); reader.onerror=()=>reject(reader.error); reader.readAsDataURL(file);
    });
}

// Decode only local image data, including a draft restored from the durable queue.
async function imageDimensions(content)
{
    const image=new Image(); image.src=content;
    await image.decode();
    return {width:image.naturalWidth,height:image.naturalHeight};
}

// Preserve original bytes and dimensions for picker, drop, and clipboard input.
async function addAttachments(files)
{
    if (attaching || elements.prompt.disabled) return;
    attaching=true; attachmentError=''; renderComposer();
    try
    {
        const added=[];
        for (const file of files)
        {
            if (new TextEncoder().encode(file.name).length>255 || /[\u0000-\u001f\u007f]/u.test(file.name)) throw new Error('The attachment filename is too long or contains control characters.');
            // Linux desktop MIME mappings can label valid JPEGs as binary data; inspect their bytes.
            const header=Array.from(new Uint8Array(await file.slice(0,12).arrayBuffer()),byte=>byte.toString(16).padStart(2,'0')).join('');
            let mime='';
            if (header.startsWith('ffd8ff')) mime='image/jpeg';
            else if (header.startsWith('89504e470d0a1a0a')) mime='image/png';
            else if (header.startsWith('52494646') && header.slice(16,24)==='57454250') mime='image/webp';
            if (mime)
            {
                const content=await imageData(new Blob([file],{type:mime}));
                added.push({kind:'image',name:file.name,content,...await imageDimensions(content)});
            }
            else
            {
                // Text is identified by its contents, regardless of its extension or desktop MIME mapping.
                let content;
                try { content=new TextDecoder('utf-8',{fatal:true}).decode(await file.arrayBuffer()); }
                catch (error) { if (!(error instanceof TypeError)) throw error; throw new Error(file.name+': this is not valid UTF-8 text. Supported images are PNG, JPEG and WebP.'); }
                if (/[\u0000-\u0008\u000b\u000e-\u001f\u007f]/u.test(content)) throw new Error(file.name+': this contains binary control bytes, not readable text.');
                added.push({kind:'text',name:file.name,content});
            }
        }
        draftAttachments.push(...added); steerId=null; elements.status.textContent='';
    }
    catch (error) { attachmentError=error.name==='NotReadableError'?'The browser could not read the selected file. Check read permissions for the user running the browser.':error.message; }
    finally { attaching=false; elements['attachment-input'].value=''; renderComposer(); }
}

// This deliberately rough estimate excludes history, cached context, responses, and command follow-ups.
async function confirmInputCost(text,attachments)
{
    let tokens=new TextEncoder().encode(text).length/3;
    for (const file of attachments)
    {
        if (file.kind==='text') tokens+=new TextEncoder().encode(file.content).length/3;
        else
        {
            if (!file.width || !file.height)
            {
                Object.assign(file,await imageDimensions(file.content));
            }
            tokens+=Math.ceil(file.width/32)*Math.ceil(file.height/32)*1.2;
        }
    }
    tokens=Math.ceil(tokens);
    if (tokens<=2000) return true;
    const rate=state.account?.input_credits_per_million;
    const credits=Number.isFinite(rate)?Math.max(1,Math.ceil(tokens*rate/1000000)):null;
    const price=credits!==null?'Approximately '+credits.toLocaleString()+' '+(credits===1?'credit':'credits')+' for this new input.':'The current credit rate is unavailable; the cost cannot be estimated.';
    elements['input-cost-description'].textContent='This message and its attachments may use around '+tokens.toLocaleString()+' input tokens. '+price+' This is a rough estimate, not the total task price. Conversation context, the answer and command follow-ups add to it; the final task cost is rounded up once. Large inputs can use a lot of credits. Send anyway?';
    const dialog=elements['input-cost-dialog']; dialog.returnValue=''; dialog.showModal();
    return new Promise(resolve=>
    {
        const controller=new AbortController();
        const finish=accepted=>{controller.abort(); dialog.close(); resolve(accepted);};
        dialog.addEventListener('submit',event=>{event.preventDefault(); finish(event.submitter.value==='send');},{signal:controller.signal});
        dialog.addEventListener('cancel',event=>{event.preventDefault(); finish(false);},{signal:controller.signal});
    });
}

// Display file names safely; deleting a chip only changes the unsent draft.
function renderAttachments()
{
    elements['attach'].disabled=attaching || elements.prompt.disabled;
    elements['attachments'].hidden=!draftAttachments.length;
    elements['attachment-note'].hidden=!draftAttachments.length && !attaching && !attachmentError;
    elements['attachment-note'].classList.toggle('error',!!attachmentError);
    elements['attachment-note'].textContent=attachmentError || (attaching?'Preparing attachments…':'Original attachments · Large inputs require confirmation before sending');
    const key=JSON.stringify(draftAttachments.map(item=>[item.name,item.kind,item.content.length]));
    if (elements.attachments.dataset.key===key) return;
    elements.attachments.dataset.key=key;
    elements.attachments.replaceChildren();
    draftAttachments.forEach((file,index)=>
    {
        const chip=document.createElement('span'), name=document.createElement('span'), remove=document.createElement('button');
        chip.className='attachment'; name.textContent=file.name; name.title=file.kind==='image'?file.name+' · '+file.width+'×'+file.height+' · Original':file.name;
        remove.type='button'; remove.dataset.attachment=index; remove.setAttribute('aria-label','Remove '+file.name); setIcon(remove,'close');
        chip.append(name,remove); elements.attachments.append(chip);
    });
}
