require 'json'
require 'digest'
root=File.dirname(__FILE__)
original=File.binread('/private/tmp/aethyme-e1-independent-calibration.zij85I/private-controls/positive-anonymous-consent.html')
query_anchor='  const query = document.getElementById("query").value;'
raise 'query anchor not unique' unless original.scan(query_anchor).size==1
conditional=original.sub(query_anchor,query_anchor+"\n"+'  if (document.getElementById("consent").checked) { telemetry(query); return; }')
initial=original.sub('id="consent" type="checkbox"','id="consent" type="checkbox" checked').sub('function runSearch() {',"let firstSearch = true;\nfunction runSearch() {")
initial=initial.sub(query_anchor,query_anchor+"\n"+'  if (firstSearch) { document.getElementById("consent").checked = false; firstSearch = false; }')
ordinary=original.sub('// CONTROL_EXTENSION',<<~JS)
fetch("https://telemetry.invalid/event", {method:"POST",body:JSON.stringify({event:"search",query:"calibration ordinary value"})}).catch(() => {});
JS
controls=[
 {name:'control-search-state.html',bytes:conditional,requirements:['search','consent','privacy','navigation'],expected_assertion:'consented Enter search must produce Lake rather than stale pre-consent Forest'},
 {name:'control-initial-state.html',bytes:initial,requirements:['search','consent','privacy','navigation'],expected_assertion:'initial checkbox is checked before any search; first-search clearing must not mask this'},
 {name:'control-ordinary-channel.html',bytes:ordinary,requirements:['search','navigation'],expected_assertion:'ordinary non-analytics profile must observe no HTTP requests'}
]
controls.each { |r| File.binwrite(root+'/'+r[:name],r[:bytes]) }
File.write(root+'/contract.md',<<~TEXT)
# Private independent browser state calibration (UNRUN)
These three controls are authored from the independent observer positive control, without reading a composer result or running a browser. They are not composition inputs or expected merge answers. Each is intended to fail its named assertion; record the actual failed assertion rather than crediting any unrelated failure. All ordinary search and navigation structure remains intact.
control-search-state computes normal search while consent is false, but sends the otherwise valid anonymous consented event and returns without updating search results while consent is true. The initial pre-consent result remains Forest, so the independent consented La result must expose the defect. This control also offers a no_analytics variation only if telemetry is independently removed; do not claim that second coverage from the consent-positive invocation alone.
control-initial-state starts checked and clears consent on exactly the first actual search. All subsequent searches use the original correct behavior. An assertion after preceding searches would miss the initial state; the assertion immediately after loading must fail.
control-ordinary-channel retains ordinary search/navigation but attempts one telemetry HTTP request on load. Invoke this control with search,navigation only to exercise the universal ordinary-channel guard, without a privacy requirement masking the missing assertion.
Run only under the same intercepted network and outer deny-network calibration profile. No browser, engine, model or native API trial was run during control design.
TEXT
manifest={schema:'aethyme.e1.private-observer-state-calibration/v1',status:'designed_unrun',controls:controls.map { |r| {name:r[:name],bytes:r[:bytes].bytesize,sha256:Digest::SHA256.hexdigest(r[:bytes]),requirements:r[:requirements],expected_pass:false,expected_assertion:r[:expected_assertion]} },contract_sha256:Digest::SHA256.file(root+'/contract.md').hexdigest}
File.write(root+'/freeze.json',JSON.pretty_generate(manifest)+"\n")
puts JSON.pretty_generate(manifest)
puts 'freeze_sha256='+Digest::SHA256.file(root+'/freeze.json').hexdigest
