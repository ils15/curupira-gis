-- 0011_rule_class_restrict.sql — apagar classe NUNCA apaga regras em silêncio.
--
-- Na 0010, `rules.class_id` nasceu com `ON DELETE CASCADE`: remover uma
-- classe apagava silenciosamente toda regra que a decidia — perda de regra
-- de domínio sem rastro. Aqui a FK é recriada com `ON DELETE RESTRICT`: o
-- banco passa a recusar o delete enquanto houver regra apontando para a
-- classe, e o repositório traduz a violação em erro tipado acionável
-- (`Error::ClassInUseByRules`). A remoção intencional (regras + classe
-- juntas) é caminho explícito da CLI (`class delete --force`, transação
-- única), nunca efeito colateral.
--
-- Idempotente: só derruba a FK se ela ainda tiver cascata (`confdeltype =
-- 'c'`) e só recria se não existir. Rodar duas vezes não muda nada.
-- Nomes de constraint no padrão do Postgres (`<tabela>_<coluna>_fkey`).

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'rules_class_id_fkey'
          AND conrelid = 'rules'::regclass
          AND confdeltype = 'c'
    ) THEN
        ALTER TABLE rules DROP CONSTRAINT rules_class_id_fkey;
    END IF;
END
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'rules_class_id_fkey'
          AND conrelid = 'rules'::regclass
    ) THEN
        ALTER TABLE rules
            ADD CONSTRAINT rules_class_id_fkey
            FOREIGN KEY (class_id) REFERENCES classes (id) ON DELETE RESTRICT;
    END IF;
END
$$;
