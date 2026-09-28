import json,subprocess,time,urllib.request,secrets,socket,pathlib,os
import argparse,tempfile
parser=argparse.ArgumentParser(description='Reproduce warm-worker raw JSON evaluation; no cluster access.')
parser.add_argument('--bundle',type=pathlib.Path,required=True)
parser.add_argument('--prompt',type=pathlib.Path)
parser.add_argument('--output',type=pathlib.Path,required=True)
options=parser.parse_args()
root=pathlib.Path(__file__).resolve().parent
schema=json.loads((root/'action.schema.json').read_text())
prompt=(options.prompt or root/'regression-prompt.txt').read_text()
fixtures=json.loads((root/'heldout.json').read_text())
cases=[c['query'] for c in fixtures]
key=secrets.token_hex(32)
with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
log=tempfile.TemporaryFile(mode='w+')
args=[str(options.bundle/'runtime/llama-server'),'-m',str(options.bundle/'model.gguf'),'--host','127.0.0.1','--port',str(port),'--api-key',key,'-c','4096','-np','1','-ngl','0','--device','none','--no-op-offload','--no-kv-offload','-t','2','--no-webui','--no-warmup','--reasoning','off','--chat-template-kwargs','{"enable_thinking":false}']
p=subprocess.Popen(args,stdout=log,stderr=log)
def req(path,body=None):
 data=None if body is None else json.dumps(body).encode();request=urllib.request.Request(f'http://127.0.0.1:{port}'+path,data,headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'});return json.load(urllib.request.urlopen(request,timeout=50))
try:
 end=time.monotonic()+40
 while time.monotonic()<end:
  if p.poll() is not None:raise RuntimeError('runtime failed; see server.log')
  try:req('/health');break
  except Exception:time.sleep(.1)
 results=[]
 for query in cases:
  start=time.monotonic()
  try:
   r=req('/v1/chat/completions',{'messages':[{'role':'system','content':prompt},{'role':'user','content':query}],'temperature':0,'max_tokens':220,'response_format':{'type':'json_schema','json_schema':{'name':'kube_action','strict':True,'schema':schema}}})
   result=r['choices'][0]['message']['content']
  except Exception as e:result=str(e)
  row={'query':query,'output':result,'seconds':round(time.monotonic()-start,3)}; row['expected']=fixtures[len(results)]['intent']; row['pass']=json.loads(result)==row['expected'];results.append(row);print(json.dumps(row),flush=True)
 options.output.write_text(json.dumps(results,indent=2))
finally:p.terminate();p.wait(timeout=10);log.close()
