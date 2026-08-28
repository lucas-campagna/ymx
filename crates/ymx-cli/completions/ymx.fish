# ymx fish completion

complete -c ymx -s h -l help -d 'Print help and exit'
complete -c ymx -l entry -d 'Component name within file to compile' -r
complete -c ymx -l from-keyword -d 'Override the from keyword' -r
complete -c ymx -l max-depth -d 'Limit on template/call recursion' -r
complete -c ymx -l format -d 'Output format' -r -f -a "json\ncompact\ndiagnostics"
complete -c ymx -l output -d 'Write output to file instead of stdout' -r -f
complete -c ymx -l plain -d 'Promote sub-namespace components and templates into global namespace'
complete -c ymx -l plain-template -d 'Promote sub-namespace templates only into global namespace'
complete -c ymx -l test -d 'Run inline _test cases instead of compiling'
complete -c ymx -f -a '*.yml' -d 'YAML files'
complete -c ymx -f -a '*.yaml' -d 'YAML files'
