/* Bundled markdown-it 15.0.3, unmodified browser build from the npm package.
Copyright (c) 2014 Vitaly Puzrin, Alex Kocharin.

Permission is hereby granted, free of charge, to any person
obtaining a copy of this software and associated documentation
files (the "Software"), to deal in the Software without
restriction, including without limitation the rights to use,
copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the
Software is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES
OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
OTHER DEALINGS IN THE SOFTWARE.
*/

'use strict';
const answerMarkdown=new markdownit({html:false,linkify:false,typographer:false}).disable('image');
const renderedAnswers=new WeakMap(),validateMarkdownLink=answerMarkdown.validateLink;
// Keep the parser's URL checks and permit only web, email and fragment links.
answerMarkdown.validateLink=href=>validateMarkdownLink(href) && /^(?:https?:\/\/|mailto:|#)/iu.test(href);
answerMarkdown.renderer.rules.link_open=(tokens,index,options,environment,renderer)=>
{
    if (!tokens[index].attrGet('href').startsWith('#')) tokens[index].attrSet('target','_blank');
    tokens[index].attrSet('rel','noopener noreferrer');
    return renderer.renderToken(tokens,index,options);
};
// Classes preserve Markdown table alignment without inline styles, which the local CSP rejects.
for (const type of ['th_open','td_open']) answerMarkdown.renderer.rules[type]=(tokens,index,options,environment,renderer)=>
{
    const token=tokens[index],alignment=/^text-align:(left|center|right)$/u.exec(token.attrGet('style') || '');
    if (alignment) token.attrSet('class','md-align-'+alignment[1]);
    token.attrs=token.attrs?.filter(([name])=>name!=='style') || null;
    return renderer.renderToken(tokens,index,options);
};
answerMarkdown.renderer.rules.table_open=()=>'<div class="markdown-table" role="region" aria-label="Table" tabindex="0"><table>\n';
answerMarkdown.renderer.rules.table_close=()=>'</table></div>\n';

// All reply paths use the same parser, including incomplete streamed Markdown and stopped answers.
function renderAnswer(element,text)
{
    if (renderedAnswers.get(element)===text) return;
    element.innerHTML=answerMarkdown.render(text);
    renderedAnswers.set(element,text);
}

