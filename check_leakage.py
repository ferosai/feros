import os
import re

directories = ['studio/api/app', 'integrations/src']
patterns_to_find = [r'organization_id', r'TenantContext', r'require_tenant', r'_org_kwargs']

    re.MULTILINE
)

leaks = []

for directory in directories:
    for root, _, files in os.walk(directory):
        for file in files:
            if not file.endswith('.py') and not file.endswith('.rs'):
                continue
            path = os.path.join(root, file)
            with open(path, 'r', encoding='utf-8') as f:
                content = f.read()

            # Simulate Copybara strip
            stripped_content = internal_block_regex.sub('', content)

            lines = stripped_content.split('\n')
            for i, line in enumerate(lines):
                # Ignore import lines for TenantContext/require_tenant in OSS mode 
                # (sometimes we leave stubs or they are in auth.py)
                for pattern in patterns_to_find:
                    if re.search(pattern, line):
                        # Filter known safe stubs or definitions
                        if file.endswith('auth.py') and ('TenantContext' in line or 'require_tenant' in line):
                            continue
                        leaks.append(f"{path}:{i+1}: {line.strip()}")

if leaks:
    print("Found potential leaks:")
    for leak in set(leaks):
        print(leak)
else:
    print("No leaks found!")
