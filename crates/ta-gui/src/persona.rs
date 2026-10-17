//! Personas da área de trabalho (paridade v0.2 com o seletor "Bem-vindo").
//!
//! Cada persona é um **preset de painéis/menus visíveis** (`PersonaFlags`)
//! aplicado ao [`crate::app_state::AppState`]. Nenhuma persona esconde o
//! canvas central — o mapa é sempre renderizado (contrato da fase).
//!
//! Módulo **sem egui**: lógica pura testável headless. A seleção persiste em
//! [`crate::workbench::WorkbenchConfig`].

/// Persona da sessão (cards do diálogo de boas-vindas).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Persona {
    /// Todos os painéis + edição (GIS completo).
    #[default]
    Sig,
    /// SIG + assistente de fluxo de trabalho.
    Assistant,
    /// Mapa + camadas + importação (sem administração).
    Operator,
    /// Conexão/administração/migrações (sem edição).
    Administrator,
}

impl Persona {
    /// Nome canônico usado na persistência (`workbench.json`).
    pub fn slug(self) -> &'static str {
        match self {
            Persona::Sig => "sig",
            Persona::Assistant => "assistant",
            Persona::Operator => "operator",
            Persona::Administrator => "administrator",
        }
    }

    /// Parse do slug persistido; inválido → `None` (chamador decide fallback).
    pub fn from_slug(s: &str) -> Option<Self> {
        match s {
            "sig" => Some(Persona::Sig),
            "assistant" => Some(Persona::Assistant),
            "operator" => Some(Persona::Operator),
            "administrator" => Some(Persona::Administrator),
            _ => None,
        }
    }

    /// Preset de visibilidade de painéis/menus da persona.
    pub fn flags(self) -> PersonaFlags {
        let base = PersonaFlags {
            inspector: true,
            attr_table: true,
            editing: true,
            import_export: true,
            project_mgmt: true,
            tools: true,
            processing: true,
            admin: true,
            plugins: true,
            wizard: false,
        };
        match self {
            // GIS completo: tudo (wizard é exclusivo do assistente).
            Persona::Sig => base,
            // Assistente = SIG + wizard de fluxo de trabalho.
            Persona::Assistant => PersonaFlags {
                wizard: true,
                ..base
            },
            // Operador: mapa + camadas + importação — sem administração,
            // sem gerência de projeto, sem processamento.
            Persona::Operator => PersonaFlags {
                project_mgmt: false,
                processing: false,
                admin: false,
                plugins: false,
                wizard: false,
                ..base
            },
            // Administrador: conexão/admin/migrações — sem edição nem
            // importação (persistência de dados não é papel deste perfil).
            Persona::Administrator => PersonaFlags {
                attr_table: false,
                editing: false,
                import_export: false,
                processing: false,
                plugins: false,
                wizard: false,
                ..base
            },
        }
    }
}

/// Feature flags de UI derivadas da persona (menus/painéis visíveis).
///
/// Menos `wizard` (que **adiciona** um painel), todos os flags são de
/// filtragem: `false` = o menu/painel correspondente não é renderizado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersonaFlags {
    /// Painel direito (CRS/estilo/bandas).
    pub inspector: bool,
    /// Painel inferior (tabela atributiva editável).
    pub attr_table: bool,
    /// Menu Editar + modos de edição (novo ponto, deletar feição).
    pub editing: bool,
    /// Importar/exportar no menu Arquivo (e atalho Ctrl+I).
    pub import_export: bool,
    /// Menu Gerenciamento de Projeto (criar/renomear/deletar).
    pub project_mgmt: bool,
    /// Menu Ferramentas (conexão, console SQL, opções).
    pub tools: bool,
    /// Menu Processamento (buffer/interseção/união PostGIS).
    pub processing: bool,
    /// Menu Administração (migrações, info do banco).
    pub admin: bool,
    /// Menu Plugins (stub do sistema futuro).
    pub plugins: bool,
    /// Assistente de fluxo de trabalho (persona Assistente).
    pub wizard: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_sao_bidirecionais() {
        for p in [
            Persona::Sig,
            Persona::Assistant,
            Persona::Operator,
            Persona::Administrator,
        ] {
            assert_eq!(Persona::from_slug(p.slug()), Some(p));
        }
        assert_eq!(Persona::from_slug("nao-existe"), None);
    }

    #[test]
    fn sig_tem_tudo_menos_wizard() {
        let f = Persona::Sig.flags();
        assert!(f.inspector && f.attr_table && f.editing && f.import_export);
        assert!(f.project_mgmt && f.tools && f.processing && f.admin && f.plugins);
        assert!(!f.wizard);
    }

    #[test]
    fn assistente_e_sig_mais_wizard() {
        let sig = Persona::Sig.flags();
        let ast = Persona::Assistant.flags();
        assert!(ast.wizard, "assistente adiciona o wizard");
        assert_eq!(
            PersonaFlags {
                wizard: false,
                ..ast
            },
            PersonaFlags {
                wizard: false,
                ..sig
            }
        );
    }

    #[test]
    fn operador_nao_tem_administracao() {
        let f = Persona::Operator.flags();
        assert!(!f.admin);
        assert!(!f.project_mgmt);
        assert!(!f.processing);
        // Mapa + camadas + importação continuam.
        assert!(f.inspector && f.attr_table && f.editing && f.import_export);
    }

    #[test]
    fn administrador_nao_tem_edicao() {
        let f = Persona::Administrator.flags();
        assert!(!f.editing);
        assert!(!f.attr_table);
        assert!(!f.import_export);
        // Conexão/admin/migrações ficam.
        assert!(f.admin && f.tools && f.inspector);
    }
}
